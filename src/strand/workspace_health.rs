//! Workspace health validation and quarantine for the Explore strand.
//!
//! Auto-discovery treats *any* directory containing `.beads/` as a candidate
//! workspace. That is deliberately promiscuous — it is how new stores join the
//! fleet without a config change — but it means discovery also picks up things
//! that are not workspaces at all: hidden test fixtures, diagnostic dump
//! directories that happen to contain a `.beads/` folder, stale duplicate
//! checkouts of a repository that is already in rotation, and stores whose
//! schema the configured backend cannot read.
//!
//! Left unexamined, each of those distorts starvation accounting (a workspace
//! that can never yield work still counts as "discovered") and re-fails every
//! Explore cycle without ever telling anyone why.
//!
//! This module validates a candidate *before* it enters fleet counts and
//! quarantines what fails. Two properties matter:
//!
//! - **Read-only.** Validation never repairs, reinitializes, or writes
//!   anything into a workspace — least of all into a store whose schema we
//!   could not read. A quarantined store is reported, not touched.
//! - **Recoverable.** Quarantine is in-memory per worker process and is
//!   released as soon as a workspace validates healthy again, so a transient
//!   lock error does not permanently exile a good store. It is deliberately
//!   *not* persisted to disk: persisting would mean writing into `.beads/` of
//!   exactly the broken workspaces we are quarantining.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many consecutive transient failures are tolerated before a workspace is
/// promoted to a quarantine event. Lock contention and a momentarily busy
/// backend are normal; three in a row is not.
pub const TRANSIENT_FAILURE_THRESHOLD: u32 = 3;

/// How often a *continuing* quarantine re-emits its event, so a problem that
/// is never fixed stays visible in telemetry instead of disappearing after the
/// first report.
pub const QUARANTINE_REMINDER_INTERVAL: u32 = 10;

/// Workspace health status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceHealthStatus {
    /// Workspace is healthy and can be scanned.
    Healthy,
    /// Workspace is quarantined due to a health issue.
    Quarantined { reason: QuarantineReason },
}

/// Reasons for quarantining a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuarantineReason {
    /// Not a git repository.
    NotAGitRepository,
    /// No bead store directory (.beads/).
    NoBeadStore,
    /// Backend configuration missing or invalid.
    BackendConfigInvalid { details: String },
    /// Bead capabilities check failed.
    CapabilitiesFailed { details: String },
    /// Duplicate repository (same identity as another workspace).
    DuplicateRepository { canonical_path: PathBuf },
    /// Hidden test fixture (excluded from discovery).
    HiddenTestFixture,
    /// Store read error.
    StoreReadError { details: String },
    /// Schema version incompatible or missing.
    SchemaIncompatible { details: String },
}

impl QuarantineReason {
    /// Stable machine-readable name for telemetry and log fields.
    pub fn slug(&self) -> &'static str {
        match self {
            QuarantineReason::NotAGitRepository => "not_a_git_repository",
            QuarantineReason::NoBeadStore => "no_bead_store",
            QuarantineReason::BackendConfigInvalid { .. } => "backend_config_invalid",
            QuarantineReason::CapabilitiesFailed { .. } => "capabilities_failed",
            QuarantineReason::DuplicateRepository { .. } => "duplicate_repository",
            QuarantineReason::HiddenTestFixture => "hidden_test_fixture",
            QuarantineReason::StoreReadError { .. } => "store_read_error",
            QuarantineReason::SchemaIncompatible { .. } => "schema_incompatible",
        }
    }

    /// For duplicate repositories, the canonical path that stays in rotation.
    pub fn canonical_path(&self) -> Option<&Path> {
        match self {
            QuarantineReason::DuplicateRepository { canonical_path } => Some(canonical_path),
            _ => None,
        }
    }

    /// Whether this reason describes a failure that can clear on its own.
    ///
    /// Transient failures are held below the reporting threshold so a single
    /// lock contention does not page an operator; permanent ones are reported
    /// immediately, because they will not get better on their own.
    pub fn is_transient(&self) -> bool {
        matches!(self, QuarantineReason::StoreReadError { .. })
    }

    /// Whether this reason means "not a workspace at all" rather than "a
    /// workspace in trouble".
    ///
    /// A hidden fixture and a directory with no store are excluded from
    /// discovery without ever being counted as unhealthy: reporting them would
    /// turn every fleet into a permanent alarm about its own test suite. The
    /// caller clears any earlier health state and moves on.
    pub fn is_excluded_shape(&self) -> bool {
        matches!(
            self,
            QuarantineReason::NoBeadStore | QuarantineReason::HiddenTestFixture
        )
    }
}

/// Workspace validation result.
#[derive(Debug, Clone)]
pub struct WorkspaceValidation {
    /// Whether the workspace passed all health checks.
    #[allow(dead_code)] // diagnostic surface; status drives the decisions today
    pub is_healthy: bool,
    /// Repository identity (for duplicate detection).
    #[allow(dead_code)] // duplicate resolution reads canonical paths instead
    pub repository_identity: Option<String>,
    /// Health status.
    pub status: WorkspaceHealthStatus,
    /// Human-readable explanation (for telemetry/diagnostics).
    #[allow(dead_code)] // diagnostic surface; reason.slug() drives telemetry today
    pub explanation: String,
}

impl WorkspaceValidation {
    /// The quarantine reason, if this workspace failed validation.
    pub fn quarantine_reason(&self) -> Option<&QuarantineReason> {
        match &self.status {
            WorkspaceHealthStatus::Quarantined { reason } => Some(reason),
            WorkspaceHealthStatus::Healthy => None,
        }
    }
}

/// In-memory quarantine registry tracking unhealthy workspaces for one worker.
///
/// Deliberately not persisted: a persisted registry would have to live inside
/// the stores it is judging, and writing into a store we could not read is the
/// one thing this module must never do.
#[derive(Debug, Clone, Default)]
pub struct QuarantineRegistry {
    /// Map of workspace path -> consecutive failure count.
    entries: HashMap<PathBuf, QuarantineEntry>,
}

/// A workspace's ongoing quarantine state.
#[derive(Debug, Clone)]
struct QuarantineEntry {
    /// How many consecutive validations have failed.
    consecutive_failures: u32,
    /// The most recent reason, used to detect reason changes.
    last_reason: QuarantineReason,
}

/// Whether an operator-facing event should be emitted for a quarantine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineTransition {
    /// First failure, or the reason changed — always report.
    Report,
    /// The failure count hit a reminder interval — report to stay visible.
    Reminder,
    /// Below the reporting threshold — stay quiet.
    Silent,
}

impl QuarantineRegistry {
    /// Check if a workspace is quarantined.
    #[allow(dead_code)] // registry API kept whole for the strands that will read it
    pub fn is_quarantined(&self, workspace: &Path) -> bool {
        self.entries.contains_key(workspace)
    }

    /// Get the quarantine reason for a workspace, if any.
    #[allow(dead_code)] // registry API kept whole for the strands that will read it
    pub fn reason(&self, workspace: &Path) -> Option<&QuarantineReason> {
        self.entries.get(workspace).map(|e| &e.last_reason)
    }

    /// Get the consecutive failure count for a workspace.
    pub fn consecutive_failures(&self, workspace: &Path) -> u32 {
        self.entries
            .get(workspace)
            .map(|e| e.consecutive_failures)
            .unwrap_or(0)
    }

    /// Record a failed validation and decide whether it should be reported.
    pub fn record_failure(
        &mut self,
        workspace: &Path,
        reason: QuarantineReason,
    ) -> QuarantineTransition {
        let transient = reason.is_transient();

        let entry = self
            .entries
            .entry(workspace.to_path_buf())
            .and_modify(|e| {
                e.consecutive_failures += 1;
            })
            .or_insert_with(|| QuarantineEntry {
                consecutive_failures: 1,
                last_reason: reason.clone(),
            });
        entry.last_reason = reason;

        let failures = entry.consecutive_failures;
        let reason_changed = failures == 1;
        let threshold_met = failures >= TRANSIENT_FAILURE_THRESHOLD;
        let reminder_due = failures % QUARANTINE_REMINDER_INTERVAL == 0;

        if transient && failures < TRANSIENT_FAILURE_THRESHOLD {
            // A single lock error is noise. Wait for a pattern.
            return if reminder_due {
                QuarantineTransition::Reminder
            } else {
                QuarantineTransition::Silent
            };
        }

        if reason_changed || threshold_met || reminder_due {
            QuarantineTransition::Report
        } else {
            QuarantineTransition::Silent
        }
    }

    /// Clear a workspace's quarantine state after a successful validation.
    ///
    /// Returns `true` if the workspace had previously been failing, so the
    /// caller can emit a recovery event.
    pub fn record_success(&mut self, workspace: &Path) -> bool {
        self.entries.remove(workspace).is_some()
    }

    /// Get all quarantined workspaces.
    #[allow(dead_code)] // registry API kept whole for the strands that will read it
    pub fn quarantined_workspaces(&self) -> Vec<(PathBuf, u32)> {
        let mut entries: Vec<(PathBuf, u32)> = self
            .entries
            .iter()
            .map(|(p, e)| (p.clone(), e.consecutive_failures))
            .collect();
        entries.sort();
        entries
    }
}

/// Validate a single workspace before it enters fleet discovery.
///
/// This is the cheap, subprocess-free half of validation: structural checks
/// only. Backend identity and inventory readability are validated by the store
/// factory when the workspace's store is opened, and surface through
/// [`classify_store_error`].
///
/// Returns a validation result carrying the repository identity needed for
/// duplicate detection. This never writes to the workspace.
pub fn validate_workspace(workspace: &Path) -> WorkspaceValidation {
    let repository_identity = repository_identity(workspace);

    // Check 1: Does it have a bead store?
    //
    // Discovery only descends into directories holding `.beads/`, so this is
    // the branch that catches explicitly configured (pinned) paths that no
    // longer exist or never had a store.
    if !workspace.join(".beads").is_dir() {
        return WorkspaceValidation {
            is_healthy: false,
            repository_identity,
            status: WorkspaceHealthStatus::Quarantined {
                reason: QuarantineReason::NoBeadStore,
            },
            explanation: format!("workspace {} has no .beads/ directory", workspace.display()),
        };
    }

    // Check 2: Is it a hidden test fixture?
    //
    // Fixture trees are shape-identical to real workspaces — that is the point
    // of a fixture — so they are recognized by name and excluded before they
    // can be counted as empty workspaces or handed to a backend.
    if is_hidden_test_fixture(workspace) {
        return WorkspaceValidation {
            is_healthy: false,
            repository_identity,
            status: WorkspaceHealthStatus::Quarantined {
                reason: QuarantineReason::HiddenTestFixture,
            },
            explanation: format!(
                "workspace {} appears to be a hidden test fixture",
                workspace.display()
            ),
        };
    }

    // Check 3: Is it a git repository?
    //
    // Checked after the store checks because `.beads/` is what makes something
    // a candidate workspace at all — a git-less directory holding diagnostics
    // under a `.beads/` folder is the common real-world failure, and naming it
    // precisely is what makes the report actionable.
    if !is_git_repository(workspace) {
        return WorkspaceValidation {
            is_healthy: false,
            repository_identity,
            status: WorkspaceHealthStatus::Quarantined {
                reason: QuarantineReason::NotAGitRepository,
            },
            explanation: format!(
                "workspace {} is not a git repository (no .git/ directory)",
                workspace.display()
            ),
        };
    }

    WorkspaceValidation {
        is_healthy: true,
        repository_identity,
        status: WorkspaceHealthStatus::Healthy,
        explanation: format!("workspace {} is healthy", workspace.display()),
    }
}

/// Whether a path looks like a git repository.
///
/// Accepts both a `.git/` directory (normal clone) and a `.git` file
/// (linked worktree or submodule), and falls back to the resolved git dir so
/// identity extraction works for both shapes.
fn is_git_repository(workspace: &Path) -> bool {
    let dotgit = workspace.join(".git");
    if dotgit.is_file() {
        return fs::read_to_string(&dotgit)
            .map(|content| {
                content
                    .lines()
                    .any(|line| line.trim_start().starts_with("gitdir:"))
            })
            .unwrap_or(false);
    }
    dotgit.is_dir()
}

/// Extract repository identity for duplicate detection.
///
/// Uses the `origin` remote URL from `.git/config`, reduced to its
/// `owner/repo` tail. Never spawns a subprocess: a stale checkout has to be
/// reported cheaply enough to check every discovery cycle.
///
/// Falls back to the directory name when no remote URL can be read, which
/// keeps a repo-local-only workspace discoverable while leaving it ineligible
/// for duplicate matching against anything but an identically named sibling.
pub fn repository_identity(workspace: &Path) -> Option<String> {
    let url = origin_url(workspace);
    match url {
        Some(url) => Some(repository_key(&url)),
        None => workspace
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from),
    }
}

/// Read `remote.origin.url` out of `.git/config` without spawning git.
///
/// Handles the normal-clone layout directly and resolves the `gitdir:` pointer
/// for worktree/submodule layouts.
fn origin_url(workspace: &Path) -> Option<String> {
    let dotgit = workspace.join(".git");
    let git_dir = if dotgit.is_file() {
        let pointer = fs::read_to_string(&dotgit).ok()?;
        let target = pointer.lines().find_map(|line| {
            let trimmed = line.trim();
            trimmed.strip_prefix("gitdir:")
        })?;
        let target = target.trim();
        let path = Path::new(target);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            workspace.join(path)
        }
    } else {
        dotgit
    };

    let config = fs::read_to_string(git_dir.join("config")).ok()?;
    parse_origin_url(&config)
}

/// Pull `url = ...` out of the `[remote "origin"]` section of a git config.
fn parse_origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin =
                line.starts_with("[remote \"origin\"]") || line.starts_with("[remote \"origin\"].");
            continue;
        }
        if !in_origin {
            continue;
        }
        if let Some(value) = line.strip_prefix("url").map(str::trim_start) {
            if let Some(value) = value.strip_prefix('=') {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

/// Reduce a git remote URL to the `owner/repo` key used for identity.
///
/// Strips credentials, protocol, host, and port, and normalizes the
/// `git@host:path` scp-like form. Comparing only the tail is what lets a
/// Forgejo checkout and its GitHub mirror collapse to one identity — the two
/// hosts are the same repository, and treating them as two workspaces is what
/// double-counts it in fleet discovery.
fn repository_key(url: &str) -> String {
    let normalized = normalize_git_url(url);
    let without_scheme = normalized
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(&normalized);
    let path = match without_scheme.split_once('/') {
        // Host is followed by a path: drop the host.
        Some((_, rest)) => rest,
        // No path at all — the whole string is the best identity we have.
        None => without_scheme,
    };
    let path = path.trim_end_matches('/');
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match segments.len() {
        0 => path.to_string(),
        1 => segments[0].to_string(),
        // owner/repo — the last two segments.
        _ => segments[segments.len() - 2..].join("/"),
    }
}

/// Normalize a git remote URL for identity comparison.
///
/// Removes credentials, lowercases, and trims a `.git` suffix and trailing
/// slash. Host and protocol are *not* collapsed here — that is
/// [`repository_key`]'s job — so the normalized URL remains usable as a
/// diagnostic.
fn normalize_git_url(url: &str) -> String {
    let mut normalized = url.trim().to_lowercase();

    // Strip credentials from scp-like and https URLs: everything before the
    // last '@' in the authority. Applied before host parsing so the host is
    // all that survives. Only stripped when the '@' sits before any path
    // separator, so a credential-looking string inside a path is left alone.
    if let Some(at_pos) = normalized.find('@') {
        let before_path = match normalized.find('/') {
            Some(slash) => slash > at_pos,
            None => true,
        };
        if before_path {
            normalized = normalized[at_pos + 1..].to_string();
        }
    }

    // git@host:owner/repo → git/host/owner/repo, so the path split below works.
    if let Some(rest) = normalized.strip_prefix("git@") {
        normalized = format!("git/{}", rest.replace(':', "/"));
    }

    if let Some(rest) = normalized.strip_prefix("ssh://") {
        normalized = rest.to_string();
    } else if let Some(rest) = normalized.strip_prefix("https://") {
        normalized = rest.to_string();
    } else if let Some(rest) = normalized.strip_prefix("http://") {
        normalized = rest.to_string();
    } else if let Some(rest) = normalized.strip_prefix("git://") {
        normalized = rest.to_string();
    }

    // Drop a port on the host.
    if let Some(slash) = normalized.find('/') {
        if let Some(colon) = normalized[..slash].rfind(':') {
            normalized = format!("{}{}", &normalized[..colon], &normalized[colon + 1..]);
        }
    }

    if normalized.ends_with(".git") {
        normalized.truncate(normalized.len() - 4);
    }
    if normalized.ends_with('/') {
        normalized.pop();
    }

    normalized
}

/// Check if a workspace is a hidden test fixture.
///
/// Fixtures are recognized by name: a leading `.` or `_` on the workspace or
/// its parent, or a name that says it is a fixture. Deliberately narrow — a
/// name merely *containing* "test" would exiling real repositories like a
/// `test-runner` service.
pub fn is_hidden_test_fixture(workspace: &Path) -> bool {
    let dir_name = match workspace.file_name().and_then(|n| n.to_str()) {
        Some(name) => name,
        None => return false,
    };

    if dir_name.starts_with('.') || dir_name.starts_with('_') {
        return true;
    }

    if let Some(parent) = workspace.parent().and_then(|p| p.file_name()) {
        if let Some(parent_str) = parent.to_str() {
            if parent_str.starts_with('.') || parent_str.starts_with('_') {
                return true;
            }
        }
    }

    let lower = dir_name.to_lowercase();
    matches!(
        lower.as_str(),
        "fixture" | "fixtures" | "test-fixture" | "test_fixture" | "testfixture" | "test-fixtures"
    ) || lower.contains("fixture") && (lower.starts_with("fixture") || lower.starts_with("test"))
}

/// Classify a store-open or inventory-read failure into a quarantine reason.
///
/// The error text is matched against the known failure signatures of
/// `bead_store::open_configured` and the backend CLI. Classification is what
/// makes the resulting event actionable — "cannot read" and "wrong schema"
/// need different repairs, and neither is "delete the store".
///
/// Never attempts a repair: the reason is returned for reporting only.
pub fn classify_store_error(error: &anyhow::Error) -> QuarantineReason {
    let details = error.to_string();
    let lower = details.to_lowercase();

    // Backend refused to identify itself, or identified as something else.
    // Both mean the configured binary and the store disagree about dialect.
    if lower.contains("backend identity mismatch")
        || lower.contains("no authoritative bead backend binding")
        || lower.contains("failed to load bead backend binding")
        || lower.contains("failed to resolve bead_cli.backend")
    {
        return QuarantineReason::BackendConfigInvalid { details };
    }

    // The capabilities handshake is the backend's own schema contract.
    if lower.contains("capabilit")
        || lower.contains("atomic_claim")
        || lower.contains("missing command")
    {
        return QuarantineReason::CapabilitiesFailed { details };
    }

    // An unreadable or wrong-shape store. Corruption markers mean the SQLite
    // image is not parseable; a schema URN in the message means it parsed but
    // is not a dialect this build can read.
    if crate::bead_store::is_corruption_error(&details)
        || lower.contains("schema")
        || lower.contains("urn:bead-rs:schema")
        || lower.contains("no such table")
        || lower.contains("no such column")
        || lower.contains("invalid bead-rs capability json")
    {
        return QuarantineReason::SchemaIncompatible { details };
    }

    // Everything else — locks, permissions, missing files, timeouts — is
    // treated as transient so a busy store is not written off on first sight.
    QuarantineReason::StoreReadError { details }
}

/// Resolve duplicate repository identities to a single canonical path.
///
/// Takes the candidate workspace list in discovery order and returns the
/// workspaces worth scanning, plus one quarantine per duplicate paired with
/// the path it duplicates. The canonical path is chosen deterministically —
/// fewest path components first (a checkout directly under the scan root beats
/// one nested in a subdirectory), then lexicographically — so every worker
/// converges on the same winner and the fleet agrees on which checkout is
/// live.
///
/// Workspaces with no repository identity are never considered duplicates of
/// anything.
pub fn resolve_duplicates(
    candidates: &[PathBuf],
) -> (Vec<PathBuf>, Vec<(PathBuf, QuarantineReason)>) {
    let mut canonical: HashMap<String, PathBuf> = HashMap::new();
    let mut duplicates: Vec<Option<(PathBuf, QuarantineReason)>> = vec![None; candidates.len()];

    for (index, path) in candidates.iter().enumerate() {
        // Workspaces with no repository identity are never duplicates.
        let Some(identity) = repository_identity(path) else {
            continue;
        };

        match canonical.get(&identity) {
            None => {
                canonical.insert(identity, path.clone());
            }
            Some(existing) => {
                let winner = canonical_choice(existing, path);
                if winner.as_path() == existing.as_path() {
                    duplicates[index] = Some((
                        path.clone(),
                        QuarantineReason::DuplicateRepository {
                            canonical_path: existing.clone(),
                        },
                    ));
                } else {
                    // The newly seen path is the better canonical one; the
                    // earlier entry becomes the duplicate instead.
                    let loser = existing.clone();
                    canonical.insert(identity, path.clone());
                    if let Some(earlier) = candidates.iter().position(|p| p == &loser) {
                        duplicates[earlier] = Some((
                            loser,
                            QuarantineReason::DuplicateRepository {
                                canonical_path: path.clone(),
                            },
                        ));
                    }
                    duplicates[index] = None;
                }
            }
        }
    }

    let mut kept = Vec::new();
    let mut reasons = Vec::new();
    for (index, path) in candidates.iter().enumerate() {
        match duplicates[index].take() {
            Some((duplicate_path, reason)) => reasons.push((duplicate_path, reason)),
            None => kept.push(path.clone()),
        }
    }

    (kept, reasons)
}

/// Pick the canonical path between two checkouts of the same repository.
fn canonical_choice(a: &Path, b: &Path) -> PathBuf {
    let a_depth = a.components().count();
    let b_depth = b.components().count();
    if a_depth != b_depth {
        return if a_depth < b_depth { a } else { b }.to_path_buf();
    }
    let (a_str, b_str) = (a.to_string_lossy(), b.to_string_lossy());
    if a_str <= b_str {
        a.to_path_buf()
    } else {
        b.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// Build a workspace-shaped fixture: a git repo with an origin remote and
    /// a `.beads/` directory.
    fn make_workspace(root: &Path, name: &str, origin: Option<&str>) -> PathBuf {
        let path = root.join(name);
        fs::create_dir_all(path.join(".beads")).expect("create .beads");
        fs::create_dir_all(path.join(".git")).expect("create .git");
        if let Some(url) = origin {
            fs::write(
                path.join(".git").join("config"),
                format!(
                    "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = {url}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n"
                ),
            )
            .expect("write git config");
        }
        path
    }

    // ── validate_workspace ────────────────────────────────────────────────

    #[test]
    fn healthy_workspace_passes_validation() {
        let root = tempdir();
        let ws = make_workspace(
            root.path(),
            "real-repo",
            Some("https://git.ardenone.com/jedarden/real-repo.git"),
        );

        let validation = validate_workspace(&ws);
        assert!(validation.is_healthy, "{:?}", validation.explanation);
        assert_eq!(validation.status, WorkspaceHealthStatus::Healthy);
        assert_eq!(
            validation.repository_identity.as_deref(),
            Some("jedarden/real-repo")
        );
    }

    #[test]
    fn missing_store_is_quarantined_not_a_git_check() {
        // A git repo with no `.beads/` reports the store problem, not git.
        let root = tempdir();
        let ws = root.path().join("no-store");
        fs::create_dir_all(ws.join(".git")).expect("git dir");

        let validation = validate_workspace(&ws);
        assert!(!validation.is_healthy);
        assert_eq!(
            validation.quarantine_reason(),
            Some(&QuarantineReason::NoBeadStore)
        );
    }

    #[test]
    fn diagnostic_dump_directory_is_not_a_git_repository() {
        // The real-world case: a stray `unknown/` directory holding only a
        // `.beads/` folder of diagnostic output. Discovery finds it, but it is
        // not a workspace.
        let root = tempdir();
        let ws = root.path().join("unknown");
        fs::create_dir_all(ws.join(".beads")).expect(".beads");
        fs::write(
            ws.join(".beads").join("query-execution-diagnostics.jsonl"),
            "{}\n",
        )
        .expect("write diagnostics");

        let validation = validate_workspace(&ws);
        assert!(!validation.is_healthy);
        assert_eq!(
            validation.quarantine_reason(),
            Some(&QuarantineReason::NotAGitRepository)
        );
    }

    #[test]
    fn hidden_test_fixture_is_excluded() {
        let root = tempdir();
        let ws = make_workspace(root.path(), ".test-fixture-store", None);

        let validation = validate_workspace(&ws);
        assert!(!validation.is_healthy);
        assert_eq!(
            validation.quarantine_reason(),
            Some(&QuarantineReason::HiddenTestFixture)
        );
    }

    #[test]
    fn fixture_under_hidden_parent_is_excluded() {
        let parent = tempdir();
        let fixture_root = parent.path().join("_fixtures");
        let ws = make_workspace(&fixture_root, "beads-store", None);

        let validation = validate_workspace(&ws);
        assert_eq!(
            validation.quarantine_reason(),
            Some(&QuarantineReason::HiddenTestFixture)
        );
    }

    #[test]
    fn repo_whose_name_merely_contains_test_is_not_a_fixture() {
        // A repository legitimately named for testing infrastructure must not
        // be exiled from discovery by a broad pattern.
        let root = tempdir();
        let ws = make_workspace(
            root.path(),
            "test-runner",
            Some("https://git.ardenone.com/jedarden/test-runner.git"),
        );

        assert!(validate_workspace(&ws).is_healthy);
    }

    #[test]
    fn linked_worktree_is_recognized_as_a_git_repository() {
        let root = tempdir();
        let ws = make_workspace(
            root.path(),
            "via-worktree",
            Some("https://git.ardenone.com/jedarden/via-worktree.git"),
        );
        // Convert to a linked-worktree layout: `.git` is a pointer file.
        fs::remove_dir_all(ws.join(".git")).expect("remove .git");
        fs::write(
            ws.join(".git"),
            "gitdir: /elsewhere/repo/.git/worktrees/via-worktree\n",
        )
        .expect("write pointer");

        assert!(is_git_repository(&ws));
        assert!(validate_workspace(&ws).is_healthy);
    }

    // ── repository identity ───────────────────────────────────────────────

    #[test]
    fn forgejo_and_github_mirror_are_one_identity() {
        // The real duplicate: the same repository checked out from Forgejo and
        // from its GitHub read-only mirror.
        let forgejo = "https://git.ardenone.com/jedarden/declarative-config.git";
        let github = "https://github.com/jedarden/declarative-config.git";

        assert_eq!(repository_key(forgejo), repository_key(github));
        assert_eq!(repository_key(forgejo), "jedarden/declarative-config");
    }

    #[test]
    fn scp_style_and_https_urls_are_one_identity() {
        assert_eq!(
            repository_key("git@github.com:jedarden/NEEDLE.git"),
            repository_key("https://github.com/jedarden/NEEDLE")
        );
    }

    #[test]
    fn credentials_are_not_part_of_identity() {
        assert_eq!(
            repository_key("https://jedarden:token@git.ardenone.com/jedarden/ARMOR.git"),
            repository_key("https://git.ardenone.com/jedarden/ARMOR.git")
        );
    }

    #[test]
    fn normalize_git_url_removes_trailing_slash() {
        assert_eq!(
            normalize_git_url("https://github.com/owner/repo/"),
            "github.com/owner/repo"
        );
    }

    #[test]
    fn identity_falls_back_to_directory_name_without_a_remote() {
        let root = tempdir();
        let ws = make_workspace(root.path(), "local-only", None);

        assert_eq!(repository_identity(&ws).as_deref(), Some("local-only"));
    }

    #[test]
    fn distinct_repositories_do_not_collide() {
        assert_ne!(
            repository_key("https://git.ardenone.com/jedarden/NEEDLE.git"),
            repository_key("https://git.ardenone.com/jedarden/FORGE.git")
        );
    }

    // ── duplicate resolution ──────────────────────────────────────────────

    #[test]
    fn duplicate_checkouts_collapse_to_the_shallower_path() {
        // Mirrors the fleet exactly: the live checkout under the scan root and
        // a stale alternate nested one level down.
        let root = tempdir();
        let live = make_workspace(
            root.path(),
            "declarative-config",
            Some("https://git.ardenone.com/jedarden/declarative-config.git"),
        );
        let nested_root = root.path().join("src");
        fs::create_dir_all(&nested_root).expect("src dir");
        let stale = make_workspace(
            &nested_root,
            "declarative-config",
            Some("https://github.com/jedarden/declarative-config.git"),
        );

        let candidates = vec![live.clone(), stale.clone()];
        let (kept, duplicates) = resolve_duplicates(&candidates);

        assert_eq!(kept, vec![live.clone()]);
        assert_eq!(duplicates.len(), 1);
        assert_eq!(
            duplicates[0].1.canonical_path(),
            Some(live.as_path()),
            "the diagnostic must name the checkout that stayed in rotation"
        );
        assert_eq!(
            duplicates[0].0, stale,
            "the stale checkout is what is named"
        );
        assert_eq!(duplicates[0].1.slug(), "duplicate_repository");
    }

    #[test]
    fn canonical_choice_is_order_independent() {
        let root = tempdir();
        let live = make_workspace(
            root.path(),
            "repo",
            Some("https://git.ardenone.com/jedarden/repo.git"),
        );
        let nested_root = root.path().join("src");
        fs::create_dir_all(&nested_root).expect("src dir");
        let stale = make_workspace(
            &nested_root,
            "repo",
            Some("https://github.com/jedarden/repo.git"),
        );

        // Either discovery order must converge on the same canonical path.
        let (first_kept, _) = resolve_duplicates(&[live.clone(), stale.clone()]);
        let (second_kept, second_duplicates) = resolve_duplicates(&[stale.clone(), live.clone()]);

        assert_eq!(first_kept, vec![live.clone()]);
        assert_eq!(second_kept, vec![live.clone()]);
        assert_eq!(second_duplicates.len(), 1);
        assert_eq!(second_duplicates[0].0, stale);
    }

    #[test]
    fn three_way_duplicate_reports_one_canonical_and_two_duplicates() {
        let root = tempdir();
        let a = make_workspace(
            root.path(),
            "repo",
            Some("https://git.ardenone.com/jedarden/repo.git"),
        );
        let deep_root = root.path().join("x").join("y");
        fs::create_dir_all(&deep_root).expect("nested dirs");
        let b = make_workspace(
            &deep_root,
            "repo",
            Some("https://github.com/jedarden/repo.git"),
        );
        let mid_root = root.path().join("z");
        fs::create_dir_all(&mid_root).expect("nested dir");
        let c = make_workspace(
            &mid_root,
            "repo",
            Some("ssh://git@git.ardenone.com/jedarden/repo.git"),
        );

        let (kept, duplicates) = resolve_duplicates(&[a.clone(), b.clone(), c.clone()]);

        assert_eq!(kept, vec![a.clone()]);
        assert_eq!(duplicates.len(), 2);
        assert!(duplicates
            .iter()
            .all(|(_, r)| r.canonical_path() == Some(a.as_path())));
    }

    #[test]
    fn workspaces_without_identity_are_never_duplicates() {
        // Two git-less directories both fall back to their own directory name,
        // so they cannot be mistaken for the same repository.
        let root = tempdir();
        let a = make_workspace(root.path(), "orphan-a", None);
        let b = make_workspace(root.path(), "orphan-b", None);

        let (kept, duplicates) = resolve_duplicates(&[a.clone(), b.clone()]);
        assert_eq!(kept.len(), 2);
        assert!(duplicates.is_empty());
    }

    // ── error classification ──────────────────────────────────────────────

    #[test]
    fn missing_binding_is_a_backend_config_problem() {
        let err = anyhow::anyhow!(
            "workspace {} has no authoritative bead backend binding; set bead_cli.backend in {}",
            "/x/repo",
            "/x/repo/.needle.yaml"
        );
        assert_eq!(classify_store_error(&err).slug(), "backend_config_invalid");
    }

    #[test]
    fn capability_gap_is_a_capabilities_problem() {
        let err = anyhow::anyhow!(
            "bead-rs capability mismatch for workspace {}: expected atomic_claim=true",
            "/x/repo"
        );
        assert_eq!(classify_store_error(&err).slug(), "capabilities_failed");
    }

    #[test]
    fn unreadable_database_is_a_schema_problem() {
        let err = anyhow::anyhow!("list failed: file is not a database");
        let reason = classify_store_error(&err);
        assert_eq!(reason.slug(), "schema_incompatible");
        assert!(
            !reason.is_transient(),
            "corruption is not going to self-heal"
        );
    }

    #[test]
    fn lock_contention_is_transient() {
        let err = anyhow::anyhow!("list failed: database is locked");
        let reason = classify_store_error(&err);
        assert_eq!(reason.slug(), "store_read_error");
        assert!(reason.is_transient());
    }

    #[test]
    fn unclassified_errors_are_treated_as_transient() {
        let err = anyhow::anyhow!("backend exited with status 137");
        let reason = classify_store_error(&err);
        assert_eq!(reason.slug(), "store_read_error");
        assert!(reason.is_transient());
    }

    // ── quarantine registry ───────────────────────────────────────────────

    #[test]
    fn permanent_failure_reports_immediately() {
        let mut registry = QuarantineRegistry::default();
        let ws = Path::new("/home/coding/unknown");

        let transition = registry.record_failure(ws, QuarantineReason::NotAGitRepository);

        assert_eq!(transition, QuarantineTransition::Report);
        assert!(registry.is_quarantined(ws));
        assert_eq!(registry.consecutive_failures(ws), 1);
    }

    #[test]
    fn transient_failures_are_held_below_the_threshold() {
        let mut registry = QuarantineRegistry::default();
        let ws = Path::new("/home/coding/busy-repo");
        let reason = || QuarantineReason::StoreReadError {
            details: "database is locked".to_string(),
        };

        assert_eq!(
            registry.record_failure(ws, reason()),
            QuarantineTransition::Silent
        );
        assert_eq!(
            registry.record_failure(ws, reason()),
            QuarantineTransition::Silent
        );
        assert_eq!(
            registry.record_failure(ws, reason()),
            QuarantineTransition::Report,
            "the third consecutive transient failure is a pattern, not noise"
        );
    }

    #[test]
    fn a_permanent_reason_upgrades_a_quiet_transient_one() {
        let mut registry = QuarantineRegistry::default();
        let ws = Path::new("/home/coding/repo");

        assert_eq!(
            registry.record_failure(
                ws,
                QuarantineReason::StoreReadError {
                    details: "locked".to_string()
                }
            ),
            QuarantineTransition::Silent
        );
        // The store turned out not to be merely busy.
        assert_eq!(
            registry.record_failure(
                ws,
                QuarantineReason::SchemaIncompatible {
                    details: "file is not a database".to_string()
                }
            ),
            QuarantineTransition::Report
        );
        assert_eq!(
            registry.reason(ws).map(|r| r.slug()),
            Some("schema_incompatible")
        );
    }

    #[test]
    fn recovery_clears_quarantine() {
        let mut registry = QuarantineRegistry::default();
        let ws = Path::new("/home/coding/repo");

        registry.record_failure(
            ws,
            QuarantineReason::StoreReadError {
                details: "locked".to_string(),
            },
        );
        assert!(
            registry.record_success(ws),
            "a recovery should be observable"
        );
        assert!(!registry.is_quarantined(ws));
        assert!(!registry.record_success(ws), "a second success is not news");
    }

    #[test]
    fn reminder_interval_keeps_a_stuck_quarantine_visible() {
        let mut registry = QuarantineRegistry::default();
        let ws = Path::new("/home/coding/stuck");
        let reason = QuarantineReason::BackendConfigInvalid {
            details: "no binding".to_string(),
        };

        assert_eq!(
            registry.record_failure(ws, reason.clone()),
            QuarantineTransition::Report
        );
        for _ in 2..QUARANTINE_REMINDER_INTERVAL {
            assert_eq!(
                registry.record_failure(ws, reason.clone()),
                QuarantineTransition::Silent
            );
        }
        assert_eq!(
            registry.record_failure(ws, reason),
            QuarantineTransition::Reminder,
            "the tenth consecutive failure re-reports"
        );
    }

    #[test]
    fn quarantined_workspaces_are_listed_deterministically() {
        let mut registry = QuarantineRegistry::default();
        let reason = QuarantineReason::NotAGitRepository;
        registry.record_failure(Path::new("/b"), reason.clone());
        registry.record_failure(Path::new("/a"), reason);

        let listed: Vec<PathBuf> = registry
            .quarantined_workspaces()
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(listed, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }
}
