//! Startup cleanup for stale disposable checkouts under `$HOME/scratch`.
//!
//! Removal is deliberately narrow: an entry must have a known fleet prefix,
//! be an independent Git clone (a real `.git/` directory), be older than the
//! configured TTL, have an `origin`, and contain neither stashes nor commits
//! absent from all remote refs. Process inspection is fail-closed and is read
//! again immediately before the destructive operation.

use std::fs::{self, File, OpenOptions};
use std::io;
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
const SWEEP_LOCK_FILE: &str = ".needle-scratch-sweep.lock";

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

/// Sweep the current user's scratch directory using the configured TTL.
pub fn sweep_home_scratch(config: &ScratchSweepConfig) -> Result<SweepOutcome> {
    if !config.enabled {
        return Ok(SweepOutcome::Disabled);
    }

    let home =
        std::env::var_os("HOME").context("HOME is not set; cannot locate scratch directory")?;
    let scratch_root = PathBuf::from(home).join("scratch");
    sweep_scratch_root(
        &scratch_root,
        config.ttl_hours,
        &ProcProcessInspector::new(PathBuf::from("/proc")),
        &GitCheckoutAuditor,
        SystemTime::now(),
    )
}

fn sweep_scratch_root(
    scratch_root: &Path,
    ttl_hours: u64,
    process_inspector: &dyn ProcessInspector,
    checkout_auditor: &dyn CheckoutAuditor,
    now: SystemTime,
) -> Result<SweepOutcome> {
    if ttl_hours == 0 {
        bail!("scratch sweep TTL must be at least one hour");
    }
    if !scratch_root.exists() {
        return Ok(SweepOutcome::ScratchDirectoryMissing {
            path: scratch_root.to_path_buf(),
        });
    }

    let root_metadata = fs::symlink_metadata(scratch_root)
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

        let Some(candidate) = qualify_candidate(&canonical_root, &entry, ttl, now, &mut report)
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

        // Destructive gate: consume a fresh, explicit process-check result
        // immediately before removal. Busy and indeterminate are both preserve.
        let deletion_gate = process_inspector.inspect(&candidate);
        match deletion_gate {
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
    let path = root.join(SWEEP_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
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
    report: &mut SweepReport,
) -> Option<PathBuf> {
    let name = entry.file_name();
    let name = name.to_str()?;
    if !DISPOSABLE_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        return None;
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
    use std::process::Command;
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
    fn removes_only_old_allowlisted_independent_clone_markers() {
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

        let report =
            completed(sweep_scratch_root(root, 48, &ClearProcesses, &SafeCheckout, now).unwrap());

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
    }

    #[test]
    fn inconclusive_process_check_preserves_candidate() {
        let temp = TempDir::new().unwrap();
        let now = SystemTime::now();
        let candidate = make_clone_marker(temp.path(), "icg-verify.abc123");
        make_old(&candidate, now);

        let report = completed(
            sweep_scratch_root(temp.path(), 48, &IndeterminateProcesses, &SafeCheckout, now)
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

        let report =
            completed(sweep_scratch_root(temp.path(), 48, &inspector, &SafeCheckout, now).unwrap());

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
        )
        .unwrap_err();

        assert!(candidate.exists());
        assert!(error.to_string().contains("at least one hour"));
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

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn clone_repo(remote: &Path, destination: &Path) {
        let output = Command::new("git")
            .arg("clone")
            .arg(remote)
            .arg(destination)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git clone failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn git_audit_removes_clean_clone_but_preserves_unpushed_commits_and_stash() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let remote = root.join("origin.git");
        let seed = root.join("seed");

        fs::create_dir(&remote).unwrap();
        git(&remote, &["init", "--bare"]);
        fs::create_dir(&seed).unwrap();
        git(&seed, &["init"]);
        git(&seed, &["config", "user.name", "Needle Test"]);
        git(
            &seed,
            &["config", "user.email", "needle-test@example.invalid"],
        );
        fs::write(seed.join("tracked.txt"), b"seed\n").unwrap();
        git(&seed, &["add", "tracked.txt"]);
        git(&seed, &["commit", "-m", "seed"]);
        let remote_arg = remote.to_string_lossy();
        git(&seed, &["remote", "add", "origin", remote_arg.as_ref()]);
        git(&seed, &["push", "-u", "origin", "HEAD"]);

        let clean = root.join("needle-clean.abc123");
        let unpushed = root.join("seam-unpushed.abc123");
        let stashed = root.join("claude-print-stashed.abc123");
        clone_repo(&remote, &clean);
        clone_repo(&remote, &unpushed);
        clone_repo(&remote, &stashed);

        // Untracked diagnostic fixtures are intentionally allowed: the TTL is
        // the retention window for failed-run investigation.
        fs::write(clean.join("test-fixture.txt"), b"diagnostic fixture\n").unwrap();

        git(&unpushed, &["config", "user.name", "Needle Test"]);
        git(
            &unpushed,
            &["config", "user.email", "needle-test@example.invalid"],
        );
        fs::write(unpushed.join("local-commit.txt"), b"local\n").unwrap();
        git(&unpushed, &["add", "local-commit.txt"]);
        git(&unpushed, &["commit", "-m", "not pushed"]);

        fs::write(stashed.join("tracked.txt"), b"stashed change\n").unwrap();
        git(&stashed, &["stash", "push", "-m", "preserve me"]);

        let now = SystemTime::now();
        for checkout in [&clean, &unpushed, &stashed] {
            make_old(checkout, now);
        }

        let report = completed(
            sweep_scratch_root(root, 48, &ClearProcesses, &GitCheckoutAuditor, now).unwrap(),
        );

        assert!(!clean.exists());
        assert!(unpushed.exists());
        assert!(stashed.exists());
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.skipped_safety, 2);
    }
}
