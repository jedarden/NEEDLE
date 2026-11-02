//! Startup cleanup for stale disposable checkouts under `$HOME/scratch` and
//! `$HOME/.needle`, plus disposable verify extracts at the top level of the
//! served workspaces.
//!
//! Removal is deliberately narrow: scratch entries need a known fleet prefix,
//! while NEEDLE-home entries may have any name except protected state roots.
//! In either location an entry must be an independent Git clone (a real
//! `.git/` directory), be older than the configured TTL, have an `origin`, and
//! contain neither stashes nor commits absent from all remote refs. Process
//! inspection is fail-closed and is read again immediately before deletion.
//!
//! Workspace candidates are the mktemp-style directories agents improvise for
//! `git archive` verification (`mktemp -d .verify-<repo>-<id>.XXXXXX`). An
//! archive extract has no `.git/` of its own, so the unpushed-commit audit
//! does not apply; removal instead requires a name match on a narrow
//! allow-list, age past the TTL, current-user ownership, a clear process gate
//! re-read immediately before deletion, and no paths tracked by the
//! workspace's index. The same name shapes are recorded in the workspace's
//! `.git/info/exclude` so fresh extracts stay out of Git scans.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use fs2::FileExt;

use crate::config::ScratchSweepConfig;

const DISPOSABLE_PREFIXES: &[&str] = &[
    "rota-",
    "armor-",
    "needle-",
    "seam-",
    "tg-pitr-",
    "icg-",
    "claude-print-",
];
const NEEDLE_HOME_RESERVED_ENTRIES: &[&str] = &["snapshots", "state", "logs"];
const SWEEP_LOCK_FILE: &str = ".needle-scratch-sweep.lock";
/// Lock file inside a workspace's `.git/` directory, so it never appears in
/// `git status` of the shared checkout.
const WORKSPACE_SWEEP_LOCK_FILE: &str = "needle-scratch-sweep.lock";

/// Top-level name prefixes identifying disposable `git archive` verification
/// extracts that agents improvise inside a workspace (`mktemp -d
/// .verify-<repo>-<id>.XXXXXX`). Deliberately narrow: nothing a maintained
/// repository keeps at its top level starts with these, and every match still
/// has to pass the TTL, ownership, process, and tracked-path gates.
const WORKSPACE_DISPOSABLE_PREFIXES: &[&str] = &[
    ".verify-",
    ".needle-head-verify.",
    ".head-verify.",
    ".baseline-",
    ".clustert-",
];

/// Git exclude patterns (anchored to the workspace root) matching the same
/// name shapes as [`is_workspace_disposable_name`]. Recorded in
/// `.git/info/exclude` so a fresh extract — one that has not yet aged past
/// the TTL — stays out of `git ls-files --others --exclude-standard` scans.
const WORKSPACE_EXCLUDE_PATTERNS: &[&str] = &[
    "/.verify-*",
    "/.needle-head-verify.*",
    "/.head-verify.*",
    "/.baseline-*",
    "/.clustert-*",
    "/.beadrs-*-gate.*",
];
const GIT_EXCLUDE_BLOCK_BEGIN: &str =
    "# BEGIN needle-scratch-sweep workspace disposable verify dirs";
const GIT_EXCLUDE_BLOCK_END: &str = "# END needle-scratch-sweep workspace disposable verify dirs";

/// Whether a top-level workspace directory name is a disposable verify
/// extract. Most shapes are plain prefixes; the bead-rs gate shape additionally
/// requires its `-gate.` marker so plausible `.beadrs-*` configuration
/// directories are never matched.
fn is_workspace_disposable_name(name: &str) -> bool {
    if name.starts_with(".beadrs-") {
        return name.contains("-gate.");
    }
    WORKSPACE_DISPOSABLE_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

#[derive(Clone, Copy)]
enum CandidateNamePolicy {
    FleetPrefixes,
    AnyName,
}

/// One checkout removed by a startup sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedCheckout {
    pub path: PathBuf,
    pub bytes_reclaimed: u64,
}

/// Observable totals for a completed startup sweep.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub entries_examined: usize,
    pub stale_candidates: usize,
    pub skipped_live: usize,
    pub skipped_safety: usize,
    pub removed: Vec<RemovedCheckout>,
    pub bytes_reclaimed: u64,
}

/// Result of attempting the host-local startup sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepOutcome {
    Disabled,
    ScratchDirectoryMissing { path: PathBuf },
    AlreadyRunning,
    Completed(SweepReport),
}

/// Sweep stale disposable checkouts from the current user's scratch and NEEDLE
/// home directories, plus disposable verify extracts from the top level of the
/// given workspace roots, using the configured TTL.
pub fn sweep_home_scratch(
    config: &ScratchSweepConfig,
    workspace_roots: &[PathBuf],
) -> Result<SweepOutcome> {
    if !config.enabled {
        return Ok(SweepOutcome::Disabled);
    }

    let home =
        std::env::var_os("HOME").context("HOME is not set; cannot locate scratch directory")?;
    let home = PathBuf::from(home);
    let now = SystemTime::now();
    let process_inspector = ProcProcessInspector::new(PathBuf::from("/proc"));
    let home_outcome = sweep_home_roots(
        &home,
        config.ttl_hours,
        &process_inspector,
        &GitCheckoutAuditor,
        now,
    )?;
    sweep_workspace_roots(
        home_outcome,
        &home,
        workspace_roots,
        config.ttl_hours,
        &process_inspector,
        &GitWorkspaceAuditor,
        now,
    )
}

/// Sweep an explicitly selected scratch directory with production safety
/// checks. This keeps callers that already own an isolated root independent
/// from process-wide `HOME` while preserving the same process and Git audits.
pub fn sweep_scratch_directory(scratch_root: &Path, ttl_hours: u64) -> Result<SweepOutcome> {
    sweep_scratch_directory_with_proc_root(scratch_root, ttl_hours, Path::new("/proc"))
}

/// Sweep an explicitly selected scratch directory while reading process state
/// from an explicitly selected procfs root.
///
/// Production callers should use [`sweep_scratch_directory`]. This variant
/// keeps the destructive process gate intact while letting integration tests
/// exercise it against an isolated procfs fixture rather than the host's
/// concurrently changing process table.
pub fn sweep_scratch_directory_with_proc_root(
    scratch_root: &Path,
    ttl_hours: u64,
    proc_root: &Path,
) -> Result<SweepOutcome> {
    sweep_scratch_root(
        scratch_root,
        ttl_hours,
        &ProcProcessInspector::new(proc_root.to_path_buf()),
        &GitCheckoutAuditor,
        SystemTime::now(),
        CandidateNamePolicy::FleetPrefixes,
    )
}

fn sweep_home_roots(
    home: &Path,
    ttl_hours: u64,
    process_inspector: &dyn ProcessInspector,
    checkout_auditor: &dyn CheckoutAuditor,
    now: SystemTime,
) -> Result<SweepOutcome> {
    let roots = [
        (home.join("scratch"), CandidateNamePolicy::FleetPrefixes),
        (home.join(".needle"), CandidateNamePolicy::AnyName),
    ];
    let mut combined = SweepReport::default();
    let mut completed_roots = 0;
    let mut already_running = false;
    let mut first_missing = None;
    let mut first_error = None;

    for (root, name_policy) in roots {
        match sweep_scratch_root(
            &root,
            ttl_hours,
            process_inspector,
            checkout_auditor,
            now,
            name_policy,
        ) {
            Ok(SweepOutcome::Completed(report)) => {
                completed_roots += 1;
                combined.entries_examined += report.entries_examined;
                combined.stale_candidates += report.stale_candidates;
                combined.skipped_live += report.skipped_live;
                combined.skipped_safety += report.skipped_safety;
                combined.bytes_reclaimed = combined
                    .bytes_reclaimed
                    .saturating_add(report.bytes_reclaimed);
                combined.removed.extend(report.removed);
            }
            Ok(SweepOutcome::ScratchDirectoryMissing { path }) => {
                first_missing.get_or_insert(path);
            }
            Ok(SweepOutcome::AlreadyRunning) => {
                already_running = true;
                tracing::info!(root = %root.display(), "scratch sweep for root is already running");
            }
            Ok(SweepOutcome::Disabled) => {
                unreachable!("root sweeps do not make independent enablement decisions")
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }

    if let Some(error) = first_error {
        return Err(error);
    }
    if completed_roots > 0 {
        return Ok(SweepOutcome::Completed(combined));
    }
    if already_running {
        return Ok(SweepOutcome::AlreadyRunning);
    }
    if let Some(path) = first_missing {
        return Ok(SweepOutcome::ScratchDirectoryMissing { path });
    }

    unreachable!("the home sweep always visits at least one root")
}

/// Fold workspace-root sweeps into the outcome of the home-roots sweep.
///
/// Roots that already are one of the home sweep's own roots keep that sweep's
/// name policy instead of being processed twice; duplicates and roots that do
/// not resolve are dropped.
fn sweep_workspace_roots(
    home_outcome: SweepOutcome,
    home: &Path,
    workspace_roots: &[PathBuf],
    ttl_hours: u64,
    process_inspector: &dyn ProcessInspector,
    tracked_auditor: &dyn WorkspaceTrackedAuditor,
    now: SystemTime,
) -> Result<SweepOutcome> {
    let reserved = [home.join("scratch"), home.join(".needle")];
    let mut roots: Vec<PathBuf> = Vec::new();
    for root in workspace_roots {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
        if reserved
            .iter()
            .any(|reserved| reserved.canonicalize().is_ok_and(|path| path == canonical))
        {
            continue;
        }
        if roots.iter().any(|known| known == &canonical) {
            continue;
        }
        roots.push(canonical);
    }
    if roots.is_empty() {
        return Ok(home_outcome);
    }

    let mut combined = None;
    let mut fallback = None;
    match home_outcome {
        SweepOutcome::Completed(report) => combined = Some(report),
        SweepOutcome::Disabled => return Ok(SweepOutcome::Disabled),
        SweepOutcome::ScratchDirectoryMissing { .. } | SweepOutcome::AlreadyRunning => {
            fallback = Some(home_outcome)
        }
    }

    let mut first_error = None;
    for root in &roots {
        match sweep_workspace_disposables(root, ttl_hours, process_inspector, tracked_auditor, now)
        {
            Ok(Some(report)) => merge_sweep_report(&mut combined, report),
            Ok(None) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }

    if let Some(error) = first_error {
        return Err(error);
    }
    match (combined, fallback) {
        (Some(report), _) => Ok(SweepOutcome::Completed(report)),
        (None, Some(outcome)) => Ok(outcome),
        (None, None) => unreachable!("home outcome is completed or retained until merged"),
    }
}

fn merge_sweep_report(base: &mut Option<SweepReport>, report: SweepReport) {
    let base = base.get_or_insert_with(SweepReport::default);
    base.entries_examined += report.entries_examined;
    base.stale_candidates += report.stale_candidates;
    base.skipped_live += report.skipped_live;
    base.skipped_safety += report.skipped_safety;
    base.bytes_reclaimed = base.bytes_reclaimed.saturating_add(report.bytes_reclaimed);
    base.removed.extend(report.removed);
}

/// Sweep disposable verify extracts from the top level of one workspace.
///
/// Returns `Ok(None)` when the pass does not apply — the root is missing, is
/// not a directory, has no `.git/` directory of its own, or another sweep of
/// the same workspace holds the lock — so callers can fold the reports that
/// did complete without manufacturing counts.
fn sweep_workspace_disposables(
    workspace_root: &Path,
    ttl_hours: u64,
    process_inspector: &dyn ProcessInspector,
    tracked_auditor: &dyn WorkspaceTrackedAuditor,
    now: SystemTime,
) -> Result<Option<SweepReport>> {
    if ttl_hours == 0 {
        bail!("scratch sweep TTL must be at least one hour");
    }
    let metadata = match fs::symlink_metadata(workspace_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect {}", workspace_root.display()))
        }
    };
    if !metadata.file_type().is_dir() {
        tracing::debug!(
            workspace = %workspace_root.display(),
            "workspace scratch-sweep pass skipped: root is not a directory"
        );
        return Ok(None);
    }
    let canonical_root = workspace_root
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", workspace_root.display()))?;
    if !canonical_root.join(".git").is_dir() {
        tracing::debug!(
            workspace = %canonical_root.display(),
            "workspace scratch-sweep pass skipped: no .git directory"
        );
        return Ok(None);
    }
    let lock_path = canonical_root.join(".git").join(WORKSPACE_SWEEP_LOCK_FILE);
    let _sweep_lock = match acquire_lock_file(&lock_path)? {
        Some(lock) => lock,
        None => {
            tracing::info!(
                workspace = %canonical_root.display(),
                "workspace scratch sweep is already running"
            );
            return Ok(None);
        }
    };

    // Fresh matching directories stay out of Git scans even before they age
    // past the TTL. Failure here is non-fatal: every removal keeps its own
    // tracked-path gate, and housekeeping must not fail worker startup.
    if let Err(error) = tracked_auditor.ensure_exclude_patterns(&canonical_root) {
        tracing::warn!(
            workspace = %canonical_root.display(),
            error = %error,
            "could not record disposable-dir patterns in .git/info/exclude"
        );
    }

    let ttl_secs = ttl_hours
        .checked_mul(60 * 60)
        .context("scratch sweep TTL is too large")?;
    let ttl = Duration::from_secs(ttl_secs);
    let mut report = SweepReport::default();
    let entries = fs::read_dir(&canonical_root)
        .with_context(|| format!("failed to read {}", canonical_root.display()))?;

    for entry_result in entries {
        report.entries_examined += 1;
        let entry = match entry_result {
            Ok(entry) => entry,
            Err(error) => {
                report.skipped_safety += 1;
                tracing::warn!(error = %error, "workspace sweep could not inspect directory entry");
                continue;
            }
        };

        let Some(candidate) =
            qualify_workspace_candidate(&canonical_root, &entry, ttl, now, &mut report)
        else {
            continue;
        };
        report.stale_candidates += 1;

        if !process_gate_allows(&candidate, process_inspector, &mut report, "initial") {
            continue;
        }

        match tracked_auditor.has_tracked_paths(&canonical_root, &candidate) {
            Ok(false) => {}
            Ok(true) => {
                report.skipped_safety += 1;
                tracing::warn!(
                    path = %candidate.display(),
                    "preserving stale workspace extract because Git tracks paths under it"
                );
                continue;
            }
            Err(error) => {
                report.skipped_safety += 1;
                tracing::warn!(
                    path = %candidate.display(),
                    error = %error,
                    "preserving stale workspace extract because its tracked-path audit failed"
                );
                continue;
            }
        }

        let bytes = match allocated_size(&candidate) {
            Ok(bytes) => bytes,
            Err(error) => {
                report.skipped_safety += 1;
                tracing::warn!(
                    path = %candidate.display(),
                    error = %error,
                    "preserving stale workspace extract because its size could not be read"
                );
                continue;
            }
        };

        // Recheck immediately before deletion; busy and indeterminate both
        // preserve the candidate.
        match process_inspector.inspect(&candidate) {
            ProcessUse::Clear => {}
            ProcessUse::Busy { pid, reason } => {
                report.skipped_live += 1;
                tracing::info!(
                    path = %candidate.display(),
                    pid,
                    reason = %reason,
                    "preserving stale workspace extract used by a live process"
                );
                continue;
            }
            ProcessUse::Indeterminate(reason) => {
                report.skipped_safety += 1;
                tracing::warn!(
                    path = %candidate.display(),
                    reason = %reason,
                    "preserving stale workspace extract because process inspection was inconclusive"
                );
                continue;
            }
        }

        match fs::remove_dir_all(&candidate) {
            Ok(()) => {
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(bytes);
                report.removed.push(RemovedCheckout {
                    path: candidate.clone(),
                    bytes_reclaimed: bytes,
                });
                tracing::info!(
                    path = %candidate.display(),
                    bytes_reclaimed = bytes,
                    "removed stale disposable workspace verify extract"
                );
            }
            Err(error) => {
                report.skipped_safety += 1;
                tracing::warn!(
                    path = %candidate.display(),
                    error = %error,
                    "failed to remove stale disposable workspace verify extract"
                );
            }
        }
    }

    tracing::info!(
        workspace = %canonical_root.display(),
        entries_examined = report.entries_examined,
        stale_candidates = report.stale_candidates,
        removed_count = report.removed.len(),
        skipped_live = report.skipped_live,
        skipped_safety = report.skipped_safety,
        bytes_reclaimed = report.bytes_reclaimed,
        "workspace disposable-extract sweep completed"
    );

    Ok(Some(report))
}

fn qualify_workspace_candidate(
    canonical_root: &Path,
    entry: &fs::DirEntry,
    ttl: Duration,
    now: SystemTime,
    report: &mut SweepReport,
) -> Option<PathBuf> {
    let name = entry.file_name();
    let name = name.to_str()?;
    if !is_workspace_disposable_name(name) {
        return None;
    }

    let metadata = match fs::symlink_metadata(entry.path()) {
        Ok(metadata) => metadata,
        Err(error) => {
            report.skipped_safety += 1;
            tracing::warn!(
                path = %entry.path().display(),
                error = %error,
                "could not inspect workspace extract candidate"
            );
            return None;
        }
    };
    // A symlink never qualifies: deletion through it could escape the
    // workspace root.
    if !metadata.file_type().is_dir() {
        return None;
    }
    if !owned_by_current_user(&metadata) {
        report.skipped_safety += 1;
        tracing::warn!(
            path = %entry.path().display(),
            "preserving workspace extract candidate owned by another user"
        );
        return None;
    }

    // Only the archive-extract shape qualifies: a directory carrying its own
    // `.git` entry is a real checkout or worktree whose unpushed state the
    // workspace's index cannot vouch for.
    if entry.path().join(".git").symlink_metadata().is_ok() {
        report.skipped_safety += 1;
        tracing::warn!(
            path = %entry.path().display(),
            "preserving workspace extract candidate that is itself a Git checkout"
        );
        return None;
    }

    let modified = match metadata.modified() {
        Ok(modified) => modified,
        Err(error) => {
            report.skipped_safety += 1;
            tracing::warn!(
                path = %entry.path().display(),
                error = %error,
                "could not read workspace extract candidate age"
            );
            return None;
        }
    };
    let Ok(age) = now.duration_since(modified) else {
        return None;
    };
    if age < ttl {
        return None;
    }

    let canonical_candidate = match entry.path().canonicalize() {
        Ok(path) => path,
        Err(error) => {
            report.skipped_safety += 1;
            tracing::warn!(
                path = %entry.path().display(),
                error = %error,
                "could not resolve workspace extract candidate"
            );
            return None;
        }
    };
    if canonical_candidate.parent() != Some(canonical_root) {
        report.skipped_safety += 1;
        tracing::warn!(
            path = %canonical_candidate.display(),
            workspace_root = %canonical_root.display(),
            "preserving workspace extract candidate that is not a direct child of the workspace root"
        );
        return None;
    }

    Some(canonical_candidate)
}

/// Workspace-side audits for the disposable-extract sweep: whether Git tracks
/// anything under a candidate, and recording the disposable-name patterns in
/// `.git/info/exclude` so fresh extracts stay out of Git scans.
trait WorkspaceTrackedAuditor {
    fn has_tracked_paths(&self, workspace_root: &Path, candidate: &Path) -> Result<bool>;
    fn ensure_exclude_patterns(&self, workspace_root: &Path) -> Result<()>;
}

/// Production auditor: asks the workspace's own Git repository.
struct GitWorkspaceAuditor;

impl WorkspaceTrackedAuditor for GitWorkspaceAuditor {
    fn has_tracked_paths(&self, workspace_root: &Path, candidate: &Path) -> Result<bool> {
        let relative = candidate.strip_prefix(workspace_root).with_context(|| {
            format!(
                "{} is not under {}",
                candidate.display(),
                workspace_root.display()
            )
        })?;
        let output = Command::new("git")
            .arg("-C")
            .arg(workspace_root)
            .arg("ls-files")
            .arg("--")
            .arg(format!(":(literal){}", relative.display()))
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .with_context(|| {
                format!(
                    "failed to run Git tracked-path audit in {}",
                    workspace_root.display()
                )
            })?;
        if !output.status.success() {
            bail!(
                "git ls-files failed with status {} in {}",
                output.status,
                workspace_root.display()
            );
        }
        Ok(!output.stdout.is_empty())
    }

    fn ensure_exclude_patterns(&self, workspace_root: &Path) -> Result<()> {
        ensure_git_exclude_patterns(workspace_root)
    }
}

/// Append the managed disposable-dir block to the workspace's
/// `.git/info/exclude`, once. Existing content is preserved.
fn ensure_git_exclude_patterns(workspace_root: &Path) -> Result<()> {
    let info_dir = workspace_root.join(".git").join("info");
    fs::create_dir_all(&info_dir)
        .with_context(|| format!("failed to prepare {}", info_dir.display()))?;
    let exclude_path = info_dir.join("exclude");
    let existing = match fs::read_to_string(&exclude_path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", exclude_path.display()))
        }
    };
    if existing
        .lines()
        .any(|line| line.trim() == GIT_EXCLUDE_BLOCK_BEGIN)
    {
        return Ok(());
    }

    let mut block = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        block.push('\n');
    }
    block.push_str(GIT_EXCLUDE_BLOCK_BEGIN);
    block.push('\n');
    for pattern in WORKSPACE_EXCLUDE_PATTERNS {
        block.push_str(pattern);
        block.push('\n');
    }
    block.push_str(GIT_EXCLUDE_BLOCK_END);
    block.push('\n');

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)
        .with_context(|| format!("failed to open {}", exclude_path.display()))?;
    file.write_all(block.as_bytes())
        .with_context(|| format!("failed to update {}", exclude_path.display()))
}

fn sweep_scratch_root(
    scratch_root: &Path,
    ttl_hours: u64,
    process_inspector: &dyn ProcessInspector,
    checkout_auditor: &dyn CheckoutAuditor,
    now: SystemTime,
    name_policy: CandidateNamePolicy,
) -> Result<SweepOutcome> {
    if ttl_hours == 0 {
        bail!("scratch sweep TTL must be at least one hour");
    }
    if !scratch_root.exists() {
        return Ok(SweepOutcome::ScratchDirectoryMissing {
            path: scratch_root.to_path_buf(),
        });
    }

    // Follow the root symlink deliberately. Fleet hosts may expose scratch on
    // another volume through this stable path; candidate deletion remains
    // constrained to direct children of the resolved root.
    let root_metadata = fs::metadata(scratch_root)
        .with_context(|| format!("failed to inspect {}", scratch_root.display()))?;
    if !root_metadata.file_type().is_dir() {
        bail!(
            "scratch root is not a directory: {}",
            scratch_root.display()
        );
    }

    let canonical_root = scratch_root
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", scratch_root.display()))?;
    let canonical_metadata = fs::symlink_metadata(&canonical_root)
        .with_context(|| format!("failed to inspect {}", canonical_root.display()))?;
    if !canonical_metadata.file_type().is_dir() {
        bail!(
            "resolved scratch root is not a directory: {}",
            canonical_root.display()
        );
    }
    let _sweep_lock = match acquire_sweep_lock(&canonical_root)? {
        Some(lock) => lock,
        None => return Ok(SweepOutcome::AlreadyRunning),
    };

    let ttl_secs = ttl_hours
        .checked_mul(60 * 60)
        .context("scratch sweep TTL is too large")?;
    let ttl = Duration::from_secs(ttl_secs);
    let mut report = SweepReport::default();
    let entries = fs::read_dir(&canonical_root)
        .with_context(|| format!("failed to read {}", canonical_root.display()))?;

    for entry_result in entries {
        report.entries_examined += 1;
        let entry = match entry_result {
            Ok(entry) => entry,
            Err(error) => {
                report.skipped_safety += 1;
                tracing::warn!(error = %error, "scratch sweep could not inspect directory entry");
                continue;
            }
        };

        let Some(candidate) =
            qualify_candidate(&canonical_root, &entry, ttl, now, name_policy, &mut report)
        else {
            continue;
        };
        report.stale_candidates += 1;

        if !process_gate_allows(&candidate, process_inspector, &mut report, "initial") {
            continue;
        }

        match checkout_auditor.audit(&candidate) {
            CheckoutAudit::Safe => {}
            CheckoutAudit::Preserve(reason) => {
                report.skipped_safety += 1;
                tracing::warn!(
                    path = %candidate.display(),
                    reason = %reason,
                    "preserving stale scratch checkout because its Git audit was not safe"
                );
                continue;
            }
        }

        let bytes = match allocated_size(&candidate) {
            Ok(bytes) => bytes,
            Err(error) => {
                report.skipped_safety += 1;
                tracing::warn!(
                    path = %candidate.display(),
                    error = %error,
                    "preserving stale scratch checkout because its size could not be read"
                );
                continue;
            }
        };

        // Recheck immediately before deletion; busy and indeterminate both
        // preserve the candidate.
        match process_inspector.inspect(&candidate) {
            ProcessUse::Clear => {}
            ProcessUse::Busy { pid, reason } => {
                report.skipped_live += 1;
                tracing::info!(
                    path = %candidate.display(),
                    pid,
                    reason = %reason,
                    "preserving stale scratch checkout used by a live process"
                );
                continue;
            }
            ProcessUse::Indeterminate(reason) => {
                report.skipped_safety += 1;
                tracing::warn!(
                    path = %candidate.display(),
                    reason = %reason,
                    "preserving stale scratch checkout because process inspection was inconclusive"
                );
                continue;
            }
        }

        match fs::remove_dir_all(&candidate) {
            Ok(()) => {
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(bytes);
                report.removed.push(RemovedCheckout {
                    path: candidate.clone(),
                    bytes_reclaimed: bytes,
                });
                tracing::info!(
                    path = %candidate.display(),
                    bytes_reclaimed = bytes,
                    "removed stale disposable scratch checkout"
                );
            }
            Err(error) => {
                report.skipped_safety += 1;
                tracing::warn!(
                    path = %candidate.display(),
                    error = %error,
                    "failed to remove stale disposable scratch checkout"
                );
            }
        }
    }

    tracing::info!(
        scratch_root = %canonical_root.display(),
        entries_examined = report.entries_examined,
        stale_candidates = report.stale_candidates,
        removed_count = report.removed.len(),
        skipped_live = report.skipped_live,
        skipped_safety = report.skipped_safety,
        bytes_reclaimed = report.bytes_reclaimed,
        "scratch startup sweep completed"
    );

    Ok(SweepOutcome::Completed(report))
}

fn acquire_sweep_lock(root: &Path) -> Result<Option<File>> {
    acquire_lock_file(&root.join(SWEEP_LOCK_FILE))
}

fn acquire_lock_file(path: &Path) -> Result<Option<File>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("failed to open scratch sweep lock {}", path.display()))?;

    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("failed to lock scratch sweep at {}", path.display())),
    }
}

fn qualify_candidate(
    canonical_root: &Path,
    entry: &fs::DirEntry,
    ttl: Duration,
    now: SystemTime,
    name_policy: CandidateNamePolicy,
    report: &mut SweepReport,
) -> Option<PathBuf> {
    let name = entry.file_name();
    let name = name.to_str()?;
    match name_policy {
        CandidateNamePolicy::FleetPrefixes => {
            if !DISPOSABLE_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
            {
                return None;
            }
        }
        CandidateNamePolicy::AnyName => {
            if NEEDLE_HOME_RESERVED_ENTRIES.contains(&name) {
                return None;
            }
        }
    }

    let metadata = match fs::symlink_metadata(entry.path()) {
        Ok(metadata) => metadata,
        Err(error) => {
            report.skipped_safety += 1;
            tracing::warn!(path = %entry.path().display(), error = %error, "could not inspect scratch candidate");
            return None;
        }
    };
    if !metadata.file_type().is_dir() {
        return None;
    }

    // A real `.git/` directory distinguishes an independent clone from a Git
    // worktree (`.git` is a file) and from similarly named scratch artifacts.
    let git_metadata = match fs::symlink_metadata(entry.path().join(".git")) {
        Ok(metadata) => metadata,
        Err(_) => return None,
    };
    if !git_metadata.file_type().is_dir() || !entry.path().join(".git/config").is_file() {
        return None;
    }

    let modified = match metadata.modified() {
        Ok(modified) => modified,
        Err(error) => {
            report.skipped_safety += 1;
            tracing::warn!(path = %entry.path().display(), error = %error, "could not read scratch candidate age");
            return None;
        }
    };
    let Ok(age) = now.duration_since(modified) else {
        return None;
    };
    if age < ttl {
        return None;
    }

    let canonical_candidate = match entry.path().canonicalize() {
        Ok(path) => path,
        Err(error) => {
            report.skipped_safety += 1;
            tracing::warn!(path = %entry.path().display(), error = %error, "could not resolve scratch candidate");
            return None;
        }
    };
    if canonical_candidate.parent() != Some(canonical_root) {
        report.skipped_safety += 1;
        tracing::warn!(
            path = %canonical_candidate.display(),
            scratch_root = %canonical_root.display(),
            "preserving scratch candidate that is not a direct child of the sweep root"
        );
        return None;
    }

    Some(canonical_candidate)
}

fn process_gate_allows(
    candidate: &Path,
    inspector: &dyn ProcessInspector,
    report: &mut SweepReport,
    check: &str,
) -> bool {
    match inspector.inspect(candidate) {
        ProcessUse::Clear => true,
        ProcessUse::Busy { pid, reason } => {
            report.skipped_live += 1;
            tracing::info!(
                path = %candidate.display(),
                pid,
                reason = %reason,
                check,
                "preserving stale scratch checkout used by a live process"
            );
            false
        }
        ProcessUse::Indeterminate(reason) => {
            report.skipped_safety += 1;
            tracing::warn!(
                path = %candidate.display(),
                reason = %reason,
                check,
                "preserving stale scratch checkout because process inspection was inconclusive"
            );
            false
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcessUse {
    Clear,
    Busy { pid: u32, reason: String },
    Indeterminate(String),
}

trait ProcessInspector {
    fn inspect(&self, candidate: &Path) -> ProcessUse;
}

struct ProcProcessInspector {
    proc_root: PathBuf,
}

impl ProcProcessInspector {
    fn new(proc_root: PathBuf) -> Self {
        Self { proc_root }
    }
}

impl ProcessInspector for ProcProcessInspector {
    fn inspect(&self, candidate: &Path) -> ProcessUse {
        let entries = match fs::read_dir(&self.proc_root) {
            Ok(entries) => entries,
            Err(error) => {
                return ProcessUse::Indeterminate(format!(
                    "failed to read {}: {error}",
                    self.proc_root.display()
                ));
            }
        };

        for entry_result in entries {
            let entry = match entry_result {
                Ok(entry) => entry,
                Err(error) => {
                    return ProcessUse::Indeterminate(format!(
                        "failed to enumerate process table: {error}"
                    ));
                }
            };
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };

            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) if process_vanished(&error) => continue,
                Err(error) => {
                    return ProcessUse::Indeterminate(format!(
                        "failed to inspect process {pid}: {error}"
                    ));
                }
            };
            if !owned_by_current_user(&metadata) {
                continue;
            }

            let comm = match fs::read_to_string(entry.path().join("comm")) {
                Ok(comm) => comm,
                Err(error) if process_vanished(&error) => continue,
                Err(error) => {
                    return ProcessUse::Indeterminate(format!(
                        "failed to read process {pid} name: {error}"
                    ));
                }
            };
            let cwd = match fs::read_link(entry.path().join("cwd")) {
                Ok(cwd) => cwd,
                Err(error) if process_vanished(&error) => continue,
                Err(error) => {
                    return ProcessUse::Indeterminate(format!(
                        "failed to read process {pid} cwd: {error}"
                    ));
                }
            };

            if cwd == candidate || cwd.starts_with(candidate) {
                return ProcessUse::Busy {
                    pid,
                    reason: format!("cwd {} is inside the checkout", cwd.display()),
                };
            }

            if matches!(comm.trim(), "cargo" | "rustc") {
                let cmdline = match fs::read(entry.path().join("cmdline")) {
                    Ok(cmdline) => cmdline,
                    Err(error) if process_vanished(&error) => continue,
                    Err(error) => {
                        return ProcessUse::Indeterminate(format!(
                            "failed to read {comm:?} process {pid} command line: {error}"
                        ));
                    }
                };
                if command_mentions_path(&cmdline, candidate) {
                    return ProcessUse::Busy {
                        pid,
                        reason: format!("{} command references the checkout", comm.trim()),
                    };
                }
            }
        }

        ProcessUse::Clear
    }
}

fn process_vanished(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

#[cfg(unix)]
fn owned_by_current_user(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    metadata.uid() == unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn owned_by_current_user(_metadata: &fs::Metadata) -> bool {
    true
}

fn command_mentions_path(cmdline: &[u8], candidate: &Path) -> bool {
    let command = String::from_utf8_lossy(cmdline);
    command.contains(candidate.to_string_lossy().as_ref())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CheckoutAudit {
    Safe,
    Preserve(String),
}

trait CheckoutAuditor {
    fn audit(&self, candidate: &Path) -> CheckoutAudit;
}

struct GitCheckoutAuditor;

impl CheckoutAuditor for GitCheckoutAuditor {
    fn audit(&self, candidate: &Path) -> CheckoutAudit {
        match audit_git_checkout(candidate) {
            Ok(()) => CheckoutAudit::Safe,
            Err(error) => CheckoutAudit::Preserve(error.to_string()),
        }
    }
}

fn audit_git_checkout(candidate: &Path) -> Result<()> {
    let top_level = run_git(candidate, &["rev-parse", "--show-toplevel"])?;
    let reported_top = PathBuf::from(String::from_utf8_lossy(&top_level.stdout).trim());
    let reported_top = reported_top.canonicalize().with_context(|| {
        format!(
            "Git reported an invalid top-level path for {}",
            candidate.display()
        )
    })?;
    if reported_top != candidate {
        bail!(
            "Git top-level is {}, not the candidate",
            reported_top.display()
        );
    }

    // An origin is required so a similarly named local repository cannot be
    // mistaken for one of the disposable clones made by fleet tooling.
    run_git(candidate, &["remote", "get-url", "origin"])
        .context("checkout has no origin remote")?;

    let stash = run_git(
        candidate,
        &["for-each-ref", "--format=%(refname)", "refs/stash"],
    )?;
    if !stash.stdout.is_empty() {
        bail!("checkout has a stash");
    }

    let unpushed = run_git(
        candidate,
        &[
            "rev-list",
            "--count",
            "--branches",
            "HEAD",
            "--not",
            "--remotes",
        ],
    )?;
    let unpushed_count = String::from_utf8_lossy(&unpushed.stdout)
        .trim()
        .parse::<u64>()
        .context("could not parse unpushed commit count")?;
    if unpushed_count != 0 {
        bail!("checkout has {unpushed_count} commit(s) absent from remote refs");
    }

    Ok(())
}

fn run_git(candidate: &Path, args: &[&str]) -> Result<Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(candidate)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .with_context(|| format!("failed to run Git audit in {}", candidate.display()))?;
    if !output.status.success() {
        bail!("Git audit command failed with status {}", output.status);
    }
    Ok(output)
}

fn allocated_size(path: &Path) -> Result<u64> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    let mut bytes = metadata_allocated_bytes(&metadata);
    if metadata.file_type().is_dir() {
        for entry in
            fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to read entry in {}", path.display()))?;
            bytes = bytes.saturating_add(allocated_size(&entry.path())?);
        }
    }
    Ok(bytes)
}

#[cfg(unix)]
fn metadata_allocated_bytes(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;

    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn metadata_allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use filetime::{set_file_mtime, FileTime};
    use tempfile::TempDir;

    use super::*;

    struct ClearProcesses;

    impl ProcessInspector for ClearProcesses {
        fn inspect(&self, _candidate: &Path) -> ProcessUse {
            ProcessUse::Clear
        }
    }

    struct SafeCheckout;

    impl CheckoutAuditor for SafeCheckout {
        fn audit(&self, _candidate: &Path) -> CheckoutAudit {
            CheckoutAudit::Safe
        }
    }

    struct IndeterminateProcesses;

    impl ProcessInspector for IndeterminateProcesses {
        fn inspect(&self, _candidate: &Path) -> ProcessUse {
            ProcessUse::Indeterminate("test inspection failure".to_string())
        }
    }

    struct ClearThenBusy {
        calls: Cell<u32>,
    }

    impl ProcessInspector for ClearThenBusy {
        fn inspect(&self, _candidate: &Path) -> ProcessUse {
            let call = self.calls.get();
            self.calls.set(call + 1);
            if call == 0 {
                ProcessUse::Clear
            } else {
                ProcessUse::Busy {
                    pid: 4242,
                    reason: "process started after initial audit".to_string(),
                }
            }
        }
    }

    fn make_clone_marker(root: &Path, name: &str) -> PathBuf {
        let checkout = root.join(name);
        fs::create_dir_all(checkout.join(".git")).unwrap();
        fs::write(checkout.join(".git/config"), b"[remote \"origin\"]\n").unwrap();
        fs::create_dir_all(checkout.join("target/debug")).unwrap();
        fs::write(checkout.join("target/debug/artifact"), vec![b'x'; 8192]).unwrap();
        checkout
    }

    fn make_old(path: &Path, now: SystemTime) {
        let old = now - Duration::from_secs(72 * 60 * 60);
        set_file_mtime(path, FileTime::from_system_time(old)).unwrap();
    }

    fn completed(outcome: SweepOutcome) -> SweepReport {
        match outcome {
            SweepOutcome::Completed(report) => report,
            other => panic!("expected completed sweep, got {other:?}"),
        }
    }

    #[test]
    fn sweeps_home_roots_with_root_specific_name_filters() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let now = SystemTime::now();

        let removable = make_clone_marker(root, "needle-verify.abc123");
        make_old(&removable, now);
        let young = make_clone_marker(root, "seam-young.abc123");

        for reference_name in ["esphome-source", "tcb-bisect", "openbao-source.20260821"] {
            let reference = make_clone_marker(root, reference_name);
            make_old(&reference, now);
        }

        let worktree = root.join("rota-worktree.abc123");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(worktree.join(".git"), b"gitdir: /somewhere/else\n").unwrap();
        make_old(&worktree, now);

        let matching_non_repo = root.join("armor-build-output.abc123");
        fs::create_dir_all(&matching_non_repo).unwrap();
        make_old(&matching_non_repo, now);

        let report = completed(
            sweep_scratch_root(
                root,
                48,
                &ClearProcesses,
                &SafeCheckout,
                now,
                CandidateNamePolicy::FleetPrefixes,
            )
            .unwrap(),
        );

        assert!(!removable.exists());
        assert!(young.exists());
        assert!(worktree.exists());
        assert!(matching_non_repo.exists());
        assert!(root.join("esphome-source").exists());
        assert!(root.join("tcb-bisect").exists());
        assert!(root.join("openbao-source.20260821").exists());
        assert_eq!(report.removed.len(), 1);
        assert!(report.bytes_reclaimed > 0);
        assert_eq!(report.removed[0].bytes_reclaimed, report.bytes_reclaimed);

        // The home startup sweep still applies the legacy prefix allowlist in
        // scratch, but accepts varied clone names under NEEDLE's state root.
        let home = TempDir::new().unwrap();
        let scratch = home.path().join("scratch");
        let needle_home = home.path().join(".needle");
        fs::create_dir_all(&scratch).unwrap();
        fs::create_dir_all(&needle_home).unwrap();
        let now = SystemTime::now();

        let scratch_clone = make_clone_marker(&scratch, "needle-verify.abc123");
        make_old(&scratch_clone, now);
        let unknown_scratch_clone = make_clone_marker(&scratch, "restore-rehearsal-scratch");
        make_old(&unknown_scratch_clone, now);

        let needle_clone = make_clone_marker(&needle_home, "restore-rehearsal-20261005");
        make_old(&needle_clone, now);
        let arbitrary_clone = make_clone_marker(&needle_home, "claudepr-session-42");
        make_old(&arbitrary_clone, now);
        let data_dump = needle_home.join("session-data-dump");
        fs::create_dir_all(&data_dump).unwrap();
        make_old(&data_dump, now);

        let reserved_roots = ["snapshots", "state", "logs"].map(|name| {
            let path = make_clone_marker(&needle_home, name);
            make_old(&path, now);
            path
        });

        let report = completed(
            sweep_home_roots(home.path(), 48, &ClearProcesses, &SafeCheckout, now).unwrap(),
        );

        assert!(!scratch_clone.exists());
        assert!(unknown_scratch_clone.exists());
        assert!(!needle_clone.exists());
        assert!(!arbitrary_clone.exists());
        assert!(data_dump.exists());
        assert!(reserved_roots.iter().all(|path| path.exists()));
        assert_eq!(report.removed.len(), 3);
        assert_eq!(report.stale_candidates, 3);
    }

    #[test]
    fn inconclusive_process_check_preserves_candidate() {
        let temp = TempDir::new().unwrap();
        let now = SystemTime::now();
        let candidate = make_clone_marker(temp.path(), "icg-verify.abc123");
        make_old(&candidate, now);

        let report = completed(
            sweep_scratch_root(
                temp.path(),
                48,
                &IndeterminateProcesses,
                &SafeCheckout,
                now,
                CandidateNamePolicy::FleetPrefixes,
            )
            .unwrap(),
        );

        assert!(candidate.exists());
        assert_eq!(report.skipped_safety, 1);
        assert!(report.removed.is_empty());
    }

    #[test]
    fn rereads_live_process_gate_immediately_before_deletion() {
        let temp = TempDir::new().unwrap();
        let now = SystemTime::now();
        let candidate = make_clone_marker(temp.path(), "tg-pitr-verify.abc123");
        make_old(&candidate, now);
        let inspector = ClearThenBusy {
            calls: Cell::new(0),
        };

        let report = completed(
            sweep_scratch_root(
                temp.path(),
                48,
                &inspector,
                &SafeCheckout,
                now,
                CandidateNamePolicy::FleetPrefixes,
            )
            .unwrap(),
        );

        assert!(candidate.exists());
        assert_eq!(inspector.calls.get(), 2);
        assert_eq!(report.skipped_live, 1);
        assert!(report.removed.is_empty());
    }

    #[test]
    fn concurrent_sweep_lock_skips_second_sweeper() {
        let temp = TempDir::new().unwrap();
        let lock = acquire_sweep_lock(temp.path()).unwrap().unwrap();

        let outcome = sweep_scratch_root(
            temp.path(),
            48,
            &ClearProcesses,
            &SafeCheckout,
            SystemTime::now(),
            CandidateNamePolicy::FleetPrefixes,
        )
        .unwrap();

        assert_eq!(outcome, SweepOutcome::AlreadyRunning);
        drop(lock);
    }

    #[test]
    fn zero_ttl_fails_closed_even_without_config_validation() {
        let temp = TempDir::new().unwrap();
        let candidate = make_clone_marker(temp.path(), "needle-zero-ttl.abc123");

        let error = sweep_scratch_root(
            temp.path(),
            0,
            &ClearProcesses,
            &SafeCheckout,
            SystemTime::now(),
            CandidateNamePolicy::FleetPrefixes,
        )
        .unwrap_err();

        assert!(candidate.exists());
        assert!(error.to_string().contains("at least one hour"));
    }

    #[cfg(unix)]
    #[test]
    fn root_symlink_to_directory_is_swept_from_canonical_target() {
        use std::os::unix::fs::symlink;

        let target = TempDir::new().unwrap();
        let link_parent = TempDir::new().unwrap();
        let link = link_parent.path().join("scratch");
        symlink(target.path(), &link).unwrap();

        let now = SystemTime::now();
        let candidate = make_clone_marker(target.path(), "needle-symlink-root.abc123");
        make_old(&candidate, now);

        let report = completed(
            sweep_scratch_root(
                &link,
                48,
                &ClearProcesses,
                &SafeCheckout,
                now,
                CandidateNamePolicy::FleetPrefixes,
            )
            .unwrap(),
        );

        assert!(!candidate.exists());
        assert_eq!(report.removed.len(), 1);
        assert!(link.exists());
    }

    #[cfg(unix)]
    #[test]
    fn root_symlink_to_file_is_rejected() {
        use std::os::unix::fs::symlink;

        let target_parent = TempDir::new().unwrap();
        let target = target_parent.path().join("not-a-directory");
        fs::write(&target, b"preserve\n").unwrap();
        let link_parent = TempDir::new().unwrap();
        let link = link_parent.path().join("scratch");
        symlink(&target, &link).unwrap();

        let error = sweep_scratch_root(
            &link,
            48,
            &ClearProcesses,
            &SafeCheckout,
            SystemTime::now(),
            CandidateNamePolicy::FleetPrefixes,
        )
        .unwrap_err();

        assert!(error.to_string().contains("not a directory"));
        assert_eq!(fs::read(&target).unwrap(), b"preserve\n");
    }

    #[cfg(unix)]
    #[test]
    fn proc_inspector_detects_cwd_and_rust_build_references() {
        use std::os::unix::fs::symlink;

        let candidate_temp = TempDir::new().unwrap();
        let candidate = candidate_temp.path().canonicalize().unwrap();
        let inside = candidate.join("target");
        fs::create_dir(&inside).unwrap();

        let cwd_proc = TempDir::new().unwrap();
        let process = cwd_proc.path().join("101");
        fs::create_dir(&process).unwrap();
        fs::write(process.join("comm"), b"bash\n").unwrap();
        fs::write(process.join("cmdline"), b"bash\0").unwrap();
        symlink(&inside, process.join("cwd")).unwrap();

        let inspector = ProcProcessInspector::new(cwd_proc.path().to_path_buf());
        assert!(matches!(
            inspector.inspect(&candidate),
            ProcessUse::Busy { pid: 101, .. }
        ));

        let cargo_proc = TempDir::new().unwrap();
        let process = cargo_proc.path().join("202");
        let outside = cargo_proc.path().join("outside");
        fs::create_dir(&process).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(process.join("comm"), b"rustc\n").unwrap();
        fs::write(
            process.join("cmdline"),
            format!("rustc\0--out-dir\0{}/target\0", candidate.display()).as_bytes(),
        )
        .unwrap();
        symlink(&outside, process.join("cwd")).unwrap();

        let inspector = ProcProcessInspector::new(cargo_proc.path().to_path_buf());
        assert!(matches!(
            inspector.inspect(&candidate),
            ProcessUse::Busy { pid: 202, .. }
        ));
    }

    struct BusyProcesses;

    impl ProcessInspector for BusyProcesses {
        fn inspect(&self, _candidate: &Path) -> ProcessUse {
            ProcessUse::Busy {
                pid: 1717,
                reason: "test process holds the directory".to_string(),
            }
        }
    }

    struct UntrackedWorkspaces;

    impl WorkspaceTrackedAuditor for UntrackedWorkspaces {
        fn has_tracked_paths(&self, _workspace_root: &Path, _candidate: &Path) -> Result<bool> {
            Ok(false)
        }

        fn ensure_exclude_patterns(&self, _workspace_root: &Path) -> Result<()> {
            Ok(())
        }
    }

    struct TrackedWorkspaces;

    impl WorkspaceTrackedAuditor for TrackedWorkspaces {
        fn has_tracked_paths(&self, _workspace_root: &Path, _candidate: &Path) -> Result<bool> {
            Ok(true)
        }

        fn ensure_exclude_patterns(&self, _workspace_root: &Path) -> Result<()> {
            Ok(())
        }
    }

    struct FailingTrackedAudit;

    impl WorkspaceTrackedAuditor for FailingTrackedAudit {
        fn has_tracked_paths(&self, _workspace_root: &Path, _candidate: &Path) -> Result<bool> {
            Err(anyhow::anyhow!("test tracked-path audit failure"))
        }

        fn ensure_exclude_patterns(&self, _workspace_root: &Path) -> Result<()> {
            Ok(())
        }
    }

    /// A workspace marker good enough for the fake-auditor tests: the pass
    /// only requires a `.git/` directory at the workspace root.
    fn make_workspace_marker(root: &Path) -> PathBuf {
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.join(".git")).unwrap();
        workspace
    }

    /// An archive-extract-shaped candidate: plain directory, no `.git`.
    fn make_extract(root: &Path, name: &str) -> PathBuf {
        let extract = root.join(name);
        fs::create_dir_all(extract.join("src")).unwrap();
        fs::write(extract.join("src").join("main.rs"), b"fn main() {}\n").unwrap();
        extract
    }

    fn sweep_workspace(
        workspace: &Path,
        ttl_hours: u64,
        inspector: &dyn ProcessInspector,
        auditor: &dyn WorkspaceTrackedAuditor,
        now: SystemTime,
    ) -> SweepReport {
        match sweep_workspace_disposables(workspace, ttl_hours, inspector, auditor, now) {
            Ok(Some(report)) => report,
            Ok(None) => panic!("workspace sweep did not apply for {}", workspace.display()),
            Err(error) => panic!("workspace sweep failed: {error}"),
        }
    }

    #[test]
    fn workspace_disposable_name_matching_is_narrow() {
        for name in [
            ".verify-readling-01a19a1f.a1b2c3",
            ".needle-head-verify.12345",
            ".head-verify.xyz",
            ".baseline-rota-42.k2k2k2",
            ".clustert-verify.9zz9zz",
            ".beadrs-census-gate.77aaa7",
        ] {
            assert!(
                is_workspace_disposable_name(name),
                "expected {name} to match"
            );
        }

        for name in [
            ".git",
            ".github",
            ".beads",
            ".needle.yaml",
            ".verify",
            ".verifyx-abc",
            ".head-verify",
            ".baseline",
            ".clustert",
            ".beadrs-config",
            ".beadrs-gate",
            ".baselinex-old",
            "verify-plain",
            ".mystery-dir",
        ] {
            assert!(
                !is_workspace_disposable_name(name),
                "expected {name} NOT to match"
            );
        }
    }

    #[test]
    fn workspace_pass_removes_stale_allowlisted_extract() {
        let temp = TempDir::new().unwrap();
        let workspace = make_workspace_marker(temp.path());
        let now = SystemTime::now();

        let stale = make_extract(&workspace, ".verify-readling-01a19a1f.abc123");
        make_old(&stale, now);
        let beadrs_gate = make_extract(&workspace, ".beadrs-census-gate.def456");
        make_old(&beadrs_gate, now);

        let report = sweep_workspace(&workspace, 48, &ClearProcesses, &UntrackedWorkspaces, now);

        assert!(!stale.exists());
        assert!(!beadrs_gate.exists());
        assert_eq!(report.stale_candidates, 2);
        assert_eq!(report.removed.len(), 2);
        assert!(report.bytes_reclaimed > 0);
        assert_eq!(report.skipped_live, 0);
        assert_eq!(report.skipped_safety, 0);
    }

    #[test]
    fn workspace_pass_keeps_fresh_extract() {
        let temp = TempDir::new().unwrap();
        let workspace = make_workspace_marker(temp.path());
        let now = SystemTime::now();

        let fresh = make_extract(&workspace, ".verify-readling-fresh.abc123");

        let report = sweep_workspace(&workspace, 48, &ClearProcesses, &UntrackedWorkspaces, now);

        assert!(fresh.exists());
        assert_eq!(report.stale_candidates, 0);
        assert!(report.removed.is_empty());
    }

    #[test]
    fn workspace_pass_keeps_extract_in_use_by_live_process() {
        let temp = TempDir::new().unwrap();
        let workspace = make_workspace_marker(temp.path());
        let now = SystemTime::now();

        let busy = make_extract(&workspace, ".verify-busy.abc123");
        make_old(&busy, now);

        let report = sweep_workspace(&workspace, 48, &BusyProcesses, &UntrackedWorkspaces, now);

        assert!(busy.exists());
        assert_eq!(report.stale_candidates, 1);
        assert_eq!(report.skipped_live, 1);
        assert!(report.removed.is_empty());
    }

    #[test]
    fn workspace_pass_keeps_extract_with_tracked_paths() {
        let temp = TempDir::new().unwrap();
        let workspace = make_workspace_marker(temp.path());
        let now = SystemTime::now();

        let tracked = make_extract(&workspace, ".verify-tracked.abc123");
        make_old(&tracked, now);

        let report = sweep_workspace(&workspace, 48, &ClearProcesses, &TrackedWorkspaces, now);

        assert!(tracked.exists());
        assert_eq!(report.stale_candidates, 1);
        assert_eq!(report.skipped_safety, 1);
        assert!(report.removed.is_empty());
    }

    #[test]
    fn workspace_pass_preserves_candidate_when_tracked_audit_fails() {
        let temp = TempDir::new().unwrap();
        let workspace = make_workspace_marker(temp.path());
        let now = SystemTime::now();

        let candidate = make_extract(&workspace, ".verify-audit-fails.abc123");
        make_old(&candidate, now);

        let report = sweep_workspace(&workspace, 48, &ClearProcesses, &FailingTrackedAudit, now);

        assert!(candidate.exists());
        assert_eq!(report.stale_candidates, 1);
        assert_eq!(report.skipped_safety, 1);
        assert!(report.removed.is_empty());
    }

    #[test]
    fn workspace_pass_keeps_non_matching_dotted_entries() {
        let temp = TempDir::new().unwrap();
        let workspace = make_workspace_marker(temp.path());
        let now = SystemTime::now();

        let kept = [
            ".mystery-cache",
            ".verify",
            ".baselinex-old",
            ".beadrs-config",
            ".git",
        ]
        .map(|name| {
            let path = workspace.join(name);
            fs::create_dir_all(&path).unwrap();
            make_old(&path, now);
            path
        });
        // A matching name that is a regular file never qualifies.
        let matching_file = workspace.join(".verify-a-file.txt");
        fs::write(&matching_file, b"not a directory\n").unwrap();
        make_old(&matching_file, now);

        let report = sweep_workspace(&workspace, 48, &ClearProcesses, &UntrackedWorkspaces, now);

        assert!(kept.iter().all(|path| path.exists()));
        assert!(matching_file.exists());
        assert_eq!(report.stale_candidates, 0);
        assert!(report.removed.is_empty());
    }

    #[test]
    fn workspace_pass_preserves_candidate_that_is_itself_a_checkout() {
        let temp = TempDir::new().unwrap();
        let workspace = make_workspace_marker(temp.path());
        let now = SystemTime::now();

        let checkout = make_clone_marker(&workspace, ".verify-real-clone.abc123");
        make_old(&checkout, now);

        let report = sweep_workspace(&workspace, 48, &ClearProcesses, &UntrackedWorkspaces, now);

        assert!(checkout.exists());
        // The checkout shape is rejected during qualification, so it never
        // counts as a stale candidate — only as a safety preserve.
        assert_eq!(report.stale_candidates, 0);
        assert_eq!(report.skipped_safety, 1);
        assert!(report.removed.is_empty());
    }

    #[test]
    fn workspace_pass_skips_roots_without_git() {
        let temp = TempDir::new().unwrap();
        let now = SystemTime::now();

        let stale = make_extract(temp.path(), ".verify-no-git-root.abc123");
        make_old(&stale, now);

        let outcome = sweep_workspace_disposables(
            temp.path(),
            48,
            &ClearProcesses,
            &UntrackedWorkspaces,
            now,
        )
        .unwrap();

        assert!(outcome.is_none());
        assert!(stale.exists());
    }

    #[test]
    fn workspace_pass_rejects_zero_ttl() {
        let temp = TempDir::new().unwrap();
        let workspace = make_workspace_marker(temp.path());

        let error = sweep_workspace_disposables(
            &workspace,
            0,
            &ClearProcesses,
            &UntrackedWorkspaces,
            SystemTime::now(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("at least one hour"));
    }

    fn run_test_git(workspace: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    fn make_git_workspace(root: &Path) -> PathBuf {
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        run_test_git(&workspace, &["init", "-q"]);
        run_test_git(
            &workspace,
            &["config", "user.email", "sweep-test@example.com"],
        );
        run_test_git(&workspace, &["config", "user.name", "sweep-test"]);
        fs::write(workspace.join("README.md"), b"workspace\n").unwrap();
        run_test_git(&workspace, &["add", "README.md"]);
        run_test_git(
            &workspace,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"],
        );
        workspace.canonicalize().unwrap()
    }

    #[test]
    fn workspace_pass_keeps_matching_directories_out_of_git_scans() {
        let temp = TempDir::new().unwrap();
        let workspace = make_git_workspace(temp.path());
        let now = SystemTime::now();

        let stale = make_extract(&workspace, ".verify-workspace-stale.abc123");
        make_old(&stale, now);
        let fresh = make_extract(&workspace, ".verify-workspace-fresh.def456");

        let report = sweep_workspace(&workspace, 48, &ClearProcesses, &GitWorkspaceAuditor, now);

        assert!(!stale.exists());
        assert!(fresh.exists());
        assert_eq!(report.removed.len(), 1);

        // The stale extract is gone and the fresh one is excluded, so the
        // untracked scan the secret-scanner hook runs stays empty.
        let others = run_test_git(&workspace, &["ls-files", "--others", "--exclude-standard"]);
        assert!(others.is_empty(), "untracked scan saw: {others}");

        let exclude = fs::read_to_string(workspace.join(".git/info/exclude")).unwrap();
        for pattern in WORKSPACE_EXCLUDE_PATTERNS {
            assert!(exclude.contains(pattern), "exclude missing {pattern}");
        }

        // Re-running the sweep must not duplicate the managed block.
        let _ = sweep_workspace(&workspace, 48, &ClearProcesses, &GitWorkspaceAuditor, now);
        let exclude = fs::read_to_string(workspace.join(".git/info/exclude")).unwrap();
        assert_eq!(
            exclude.matches(GIT_EXCLUDE_BLOCK_BEGIN).count(),
            1,
            "managed exclude block was duplicated: {exclude}"
        );

        // Once the fresh extract ages past the TTL it is removed too.
        make_old(&fresh, now);
        let report = sweep_workspace(&workspace, 48, &ClearProcesses, &GitWorkspaceAuditor, now);
        assert!(!fresh.exists());
        assert_eq!(report.stale_candidates, 1);
        assert_eq!(report.removed.len(), 1);
        let others = run_test_git(&workspace, &["ls-files", "--others", "--exclude-standard"]);
        assert!(others.is_empty(), "untracked scan saw: {others}");
    }

    #[test]
    fn workspace_roots_dedupe_and_skip_home_sweep_roots() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let scratch = home.join("scratch");
        let needle_home = home.join(".needle");
        fs::create_dir_all(&scratch).unwrap();
        fs::create_dir_all(&needle_home).unwrap();

        let workspace = make_workspace_marker(temp.path());
        let stale = make_extract(&workspace, ".verify-dedupe.abc123");
        make_old(&stale, SystemTime::now());
        let scratch_extract = make_extract(&scratch, ".verify-in-scratch.abc123");
        make_old(&scratch_extract, SystemTime::now());

        let outcome =
            sweep_home_roots(&home, 48, &ClearProcesses, &SafeCheckout, SystemTime::now()).unwrap();
        let outcome = sweep_workspace_roots(
            outcome,
            &home,
            &[
                workspace.clone(),
                scratch.clone(),
                needle_home.clone(),
                workspace.clone(),
            ],
            48,
            &ClearProcesses,
            &UntrackedWorkspaces,
            SystemTime::now(),
        )
        .unwrap();

        // The workspace is swept once; the scratch root keeps its own policy
        // and is not reprocessed with the workspace allow-list (the extract
        // there is not a clone, so the home sweep leaves it alone too).
        let report = completed(outcome);
        assert!(!stale.exists());
        assert!(scratch_extract.exists());
        assert_eq!(report.removed.len(), 1);
    }
}
