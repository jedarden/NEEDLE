//! `F7_STALE_TREE_DRIFT`: a shared checkout whose tree lags its own HEAD.
//!
//! Between 2026-10-07 and 10-16 fleet workers advanced the shared `~/NEEDLE`
//! `main` with `git update-ref refs/heads/main <origin/main>` — "does not
//! checkout or overwrite the shared dirty worktree" — about twenty times. Each
//! move left the index and working tree where they were, so every file the
//! skipped commits changed now read as a staged *revert* of those commits. By
//! the time a person looked, the checkout was 85 files and 9,959 lines behind
//! HEAD (Weft, file contention and the LOOM config absent on disk), and any
//! pathspec commit of one of those files silently undid somebody else's
//! commit. Workers noticed and "preserved" it; nothing reported it.
//!
//! The signature that separates this from ordinary in-flight work is cheap to
//! check: a file somebody is editing holds content no commit ever had, while a
//! file left behind by a ref move holds content an *older commit* had. This
//! rule reports a workspace with at least `audit.tree_drift.min_paths` tracked
//! paths (outside `.beads/`) whose index or working-tree content differs from
//! HEAD and matches a blob — or the absence — that the path held in the last
//! `audit.tree_drift.history_commits` commits touching it.
//!
//! Collection runs `git` read-only with `--no-optional-locks`, so it never
//! takes the index lock a worker in the same checkout may be waiting on. The
//! classification is a pure function of what collection gathered, so it is
//! provable against fixtures without spawning anything.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use super::{AuditContext, Collected, Finding, Predicate};

/// Rule id for a checkout whose tracked files match older commits than HEAD.
pub const F7_STALE_TREE_DRIFT: &str = "F7_STALE_TREE_DRIFT";

/// The finding's subject. Constant so the evidence signature — and so the
/// filed bead — is one per workspace however the stale set moves.
const SUBJECT: &str = "shared-checkout";

/// Changed paths examined per workspace. A checkout with more is still
/// reported; only the examination stops, which can only under-count.
const MAX_PATHS_EXAMINED: usize = 2000;

/// Stale paths named in a finding before the rest are counted.
const LISTED_PATHS: usize = 20;

/// Which side of the checkout holds the older content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StaleSide {
    Index,
    Worktree,
    Both,
}

impl StaleSide {
    fn as_str(&self) -> &'static str {
        match self {
            StaleSide::Index => "index",
            StaleSide::Worktree => "worktree",
            StaleSide::Both => "index+worktree",
        }
    }
}

/// One path whose content is an older committed state than HEAD's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalePath {
    pub path: String,
    pub side: StaleSide,
    /// The newest commit (within the examined history) in which the path
    /// held the content now on that side, when one could be named.
    pub matches_commit: Option<String>,
}

/// What collection found in one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeDrift {
    /// HEAD at collection time.
    pub head: String,
    /// Tracked non-`.beads` paths whose index or worktree differs from HEAD.
    pub differing: usize,
    /// The subset holding an older committed state, sorted by path.
    pub stale: Vec<StalePath>,
}

/// One path's content on each side. `None` means absent there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathState {
    pub path: String,
    pub head: Option<String>,
    pub index: Option<String>,
    pub worktree: Option<String>,
}

/// The states one path has held in the examined history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathHistory {
    /// Blob id -> the newest commit in which the path held it, as far as the
    /// examined history shows (a blob seen only as a diff's old side maps to
    /// the commit that replaced it, i.e. the state just before that commit).
    pub blobs: BTreeMap<String, String>,
    /// Where the path's absence sits in the examined history, when it was
    /// added (`<commit>^`, absent just before it) or deleted (`<commit>`) in
    /// it: absence is then a state the path has held.
    pub absent_around: Option<String>,
}

/// Every path whose index or worktree content equals a state the path held
/// in `history` but not HEAD's — the left-behind tree a ref move produces.
///
/// A path whose content no examined commit ever had is in-flight work and is
/// not reported, whatever else is true of it.
pub fn classify(states: &[PathState], history: &BTreeMap<String, PathHistory>) -> Vec<StalePath> {
    let empty = PathHistory::default();
    let mut stale = Vec::new();
    for state in states {
        let history = history.get(&state.path).unwrap_or(&empty);
        let older = |content: &Option<String>| -> Option<Option<String>> {
            if content == &state.head {
                return None;
            }
            match content {
                Some(blob) => history.blobs.get(blob).map(|commit| Some(commit.clone())),
                None => history
                    .absent_around
                    .as_ref()
                    .map(|commit| Some(commit.clone())),
            }
        };
        let index = older(&state.index);
        // A file missing on disk while the index still holds it is an
        // ordinary deletion (an `rm`, a cleanup sweep of committed logs),
        // not a tree a ref move left behind — that leaves the index behind
        // too. Only an absence the index shares is counted.
        let worktree = if state.worktree.is_none() && state.index.is_some() {
            None
        } else {
            older(&state.worktree)
        };
        let (side, matches_commit) = match (index, worktree) {
            (Some(commit), Some(_)) => (StaleSide::Both, commit),
            (Some(commit), None) => (StaleSide::Index, commit),
            (None, Some(commit)) => (StaleSide::Worktree, commit),
            (None, None) => continue,
        };
        stale.push(StalePath {
            path: state.path.clone(),
            side,
            matches_commit,
        });
    }
    stale.sort_by(|a, b| a.path.cmp(&b.path));
    stale
}

/// Whether a path belongs to the bead store, which this rule never judges:
/// `.beads/` churns on every mutation and is published by its own pipeline.
fn is_bead_state(path: &str) -> bool {
    path == ".beads" || path.starts_with(".beads/")
}

/// NUL-separated names, as `git diff --name-only -z` prints them.
pub fn parse_names_z(stdout: &str) -> Vec<String> {
    stdout
        .split('\0')
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// `git ls-files -s -z` / `git ls-tree -r -z` records: `<meta> <blob>...\t<path>`
/// where the blob is the second whitespace field. Stage-0 index entries and
/// tree blobs both have that shape; a conflicted index entry (stage != 0) is
/// not a committed state and is skipped.
pub fn parse_blob_records_z(stdout: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for record in stdout.split('\0').filter(|record| !record.is_empty()) {
        let Some((meta, path)) = record.split_once('\t') else {
            continue;
        };
        let fields: Vec<&str> = meta.split_whitespace().collect();
        // ls-files -s: <mode> <blob> <stage>; ls-tree: <mode> <type> <blob>.
        let blob = match fields.as_slice() {
            [_, blob, stage] if stage.len() == 1 => {
                if *stage != "0" {
                    continue;
                }
                *blob
            }
            [_, kind, blob] if *kind == "blob" => *blob,
            _ => continue,
        };
        out.insert(path.to_string(), blob.to_string());
    }
    out
}

/// `git log --raw --no-abbrev --format='COMMIT %H'` output: per path, every
/// blob it held and the newest commit holding each, newest commit first.
pub fn parse_log_raw(stdout: &str) -> BTreeMap<String, PathHistory> {
    const ZERO: &str = "0000000000000000000000000000000000000000";
    let mut out: BTreeMap<String, PathHistory> = BTreeMap::new();
    let mut commit = String::new();
    for line in stdout.lines() {
        if let Some(sha) = line.strip_prefix("COMMIT ") {
            commit = sha.trim().to_string();
            continue;
        }
        let Some(raw) = line.strip_prefix(':') else {
            continue;
        };
        let Some((meta, path)) = raw.split_once('\t') else {
            continue;
        };
        let fields: Vec<&str> = meta.split_whitespace().collect();
        let [_, _, old, new, status] = fields.as_slice() else {
            continue;
        };
        let history = out.entry(path.to_string()).or_default();
        // Log order is newest first, so the first sighting of a blob names
        // the newest commit that held it.
        if *new != ZERO {
            history
                .blobs
                .entry(new.to_string())
                .or_insert_with(|| commit.clone());
        }
        if *old != ZERO {
            history
                .blobs
                .entry(old.to_string())
                .or_insert_with(|| format!("{commit}^"));
        }
        if status.starts_with('A') {
            history
                .absent_around
                .get_or_insert_with(|| format!("{commit}^"));
        } else if status.starts_with('D') {
            history.absent_around.get_or_insert_with(|| commit.clone());
        }
    }
    out
}

/// Run one read-only git command in `workspace`, returning stdout.
fn git(workspace: &Path, args: &[&str], stdin: Option<&str>) -> Result<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(workspace)
        .args(["--no-optional-locks", "-c", "core.quotepath=off"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input.as_bytes())
            .with_context(|| format!("failed to feed git {}", args.join(" ")))?;
    }
    let output = child
        .wait_with_output()
        .with_context(|| format!("failed to wait for git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} exited with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether `git` answers `rev-parse -q --verify <rev>` in `workspace`.
fn resolves(workspace: &Path, rev: &str) -> bool {
    git(workspace, &["rev-parse", "-q", "--verify", rev], None).is_ok()
}

/// Gather and classify one workspace's tree against its HEAD.
///
/// `Ok(None)` for a directory with no commit to drift from (not a git
/// checkout, or an unborn branch). `Err` for a checkout that could not be
/// judged, including one mid-merge or mid-rebase, whose index legitimately
/// holds other commits' content.
pub fn collect(workspace: &Path, history_commits: u64) -> Result<Option<TreeDrift>> {
    if !workspace.join(".git").exists() {
        return Ok(None);
    }
    let Ok(head) = git(
        workspace,
        &["rev-parse", "-q", "--verify", "HEAD^{commit}"],
        None,
    ) else {
        return Ok(None);
    };
    let head = head.trim().to_string();
    for marker in [
        "MERGE_HEAD",
        "REBASE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ] {
        if resolves(workspace, marker) {
            bail!("{marker} is present: a merge, rebase or pick is in progress");
        }
    }

    let mut paths: BTreeSet<String> = BTreeSet::new();
    for cached in [false, true] {
        let mut args = vec!["diff", "--name-only", "-z", "--no-renames"];
        if cached {
            args.push("--cached");
        }
        args.extend([head.as_str(), "--"]);
        paths.extend(parse_names_z(&git(workspace, &args, None)?));
    }
    let paths: Vec<String> = paths
        .into_iter()
        .filter(|path| !is_bead_state(path))
        .collect();
    let differing = paths.len();
    if paths.is_empty() {
        return Ok(Some(TreeDrift {
            head,
            differing,
            stale: Vec::new(),
        }));
    }
    let examined: Vec<&str> = paths
        .iter()
        .take(MAX_PATHS_EXAMINED)
        .map(String::as_str)
        .collect();

    let with_paths = |prefix: &[&str]| -> Vec<String> {
        prefix
            .iter()
            .map(|arg| arg.to_string())
            .chain(std::iter::once("--".to_string()))
            .chain(examined.iter().map(|path| path.to_string()))
            .collect()
    };
    let run = |args: Vec<String>| -> Result<String> {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        git(workspace, &refs, None)
    };

    let at_head = parse_blob_records_z(&run(with_paths(&["ls-tree", "-r", "-z", &head]))?);
    let in_index = parse_blob_records_z(&run(with_paths(&["ls-files", "-s", "-z"]))?);

    // Hash regular files on disk the way git would store them (filters and
    // all); a missing file is absent, and anything else (a symlink, a
    // directory where a file was) is left unjudged rather than guessed at.
    let on_disk: Vec<&str> = examined
        .iter()
        .copied()
        .filter(|path| {
            std::fs::symlink_metadata(workspace.join(path)).is_ok_and(|meta| meta.is_file())
        })
        .collect();
    let mut in_worktree: BTreeMap<String, String> = BTreeMap::new();
    if !on_disk.is_empty() {
        let mut input = on_disk.join("\n");
        input.push('\n');
        let hashed = git(workspace, &["hash-object", "--stdin-paths"], Some(&input))?;
        for (path, blob) in on_disk.iter().zip(hashed.lines()) {
            in_worktree.insert(path.to_string(), blob.trim().to_string());
        }
    }

    let limit = format!("-n{history_commits}");
    let history = parse_log_raw(&run(with_paths(&[
        "log",
        &limit,
        "--no-renames",
        "--raw",
        "--no-abbrev",
        "--format=COMMIT %H",
        &head,
    ]))?);

    let states: Vec<PathState> = examined
        .iter()
        .filter_map(|path| {
            let exists = workspace.join(path).symlink_metadata().is_ok();
            let worktree = match in_worktree.get(*path) {
                Some(blob) => Some(blob.clone()),
                // Present but unhashed (not a regular file): leave unjudged
                // by reporting the HEAD state, which never reads as stale.
                None if exists => return None,
                None => None,
            };
            Some(PathState {
                path: path.to_string(),
                head: at_head.get(*path).cloned(),
                index: in_index.get(*path).cloned(),
                worktree,
            })
        })
        .collect();

    Ok(Some(TreeDrift {
        head,
        differing,
        stale: classify(&states, &history),
    }))
}

/// A checkout with at least `audit.tree_drift.min_paths` tracked paths
/// holding an older committed state than HEAD.
pub struct StaleTreeDrift;

impl Predicate for StaleTreeDrift {
    fn id(&self) -> &'static str {
        F7_STALE_TREE_DRIFT
    }

    fn description(&self) -> &'static str {
        "a checkout's index or worktree holds older committed content than its HEAD: a ref was moved without the tree"
    }

    fn check(&self, ctx: &AuditContext) -> Result<Vec<Finding>> {
        let min_paths = ctx.config.audit.tree_drift.min_paths;
        let mut findings = Vec::new();
        for (workspace, collected) in &ctx.tree_drift {
            // Not collected is reported as an unavailable input by the run.
            let Collected::Available(Some(drift)) = collected else {
                continue;
            };
            if (drift.stale.len() as u64) < min_paths.max(1) {
                continue;
            }
            findings.push(Finding::violation(
                self.id(),
                workspace.clone(),
                SUBJECT,
                detail(drift),
                drift.stale.len() as u64,
            ));
        }
        Ok(findings)
    }
}

/// A commit abbreviated for the detail, keeping a trailing `^` ("the state
/// just before this commit").
fn short(commit: &str) -> String {
    let (sha, parent) = match commit.strip_suffix('^') {
        Some(sha) => (sha, "^"),
        None => (commit, ""),
    };
    format!("{}{parent}", sha.get(..12).unwrap_or(sha))
}

fn detail(drift: &TreeDrift) -> String {
    let count = |side: StaleSide| {
        drift
            .stale
            .iter()
            .filter(|path| path.side == side || path.side == StaleSide::Both)
            .count()
    };
    let mut listed: Vec<String> = drift
        .stale
        .iter()
        .take(LISTED_PATHS)
        .map(|path| match &path.matches_commit {
            Some(commit) => format!("{} ({} = {})", path.path, path.side.as_str(), short(commit)),
            None => format!("{} ({})", path.path, path.side.as_str()),
        })
        .collect();
    if drift.stale.len() > LISTED_PATHS {
        listed.push(format!("(+{} more)", drift.stale.len() - LISTED_PATHS));
    }
    format!(
        "{} tracked path(s) outside .beads/ hold content an older commit had instead of HEAD {} \
         ({} in the index, {} in the worktree; {} path(s) differ from HEAD in all). The branch \
         ref moved without the tree — git update-ref, reset --soft or branch -f in a shared \
         checkout — so each of these reads as a staged revert of a newer commit, and a pathspec \
         commit of any of them silently undoes it. Do not commit from this tree: salvage genuine \
         in-flight edits, then bring the listed paths back to HEAD. Paths: {}",
        drift.stale.len(),
        short(&drift.head),
        count(StaleSide::Index),
        count(StaleSide::Worktree),
        drift.differing,
        listed.join(", "),
    )
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn state(
        path: &str,
        head: Option<&str>,
        index: Option<&str>,
        worktree: Option<&str>,
    ) -> PathState {
        PathState {
            path: path.to_string(),
            head: head.map(str::to_string),
            index: index.map(str::to_string),
            worktree: worktree.map(str::to_string),
        }
    }

    /// The `git log --raw` shape for three commits touching two files:
    /// `c3` (HEAD) changed a.rs, `c2` added b.rs, `c1` changed a.rs.
    const LOG: &str = "\
COMMIT c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3
:100644 100644 a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2 a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3 M\tsrc/a.rs

COMMIT c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2
:000000 100644 0000000000000000000000000000000000000000 b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1 A\tsrc/b.rs

COMMIT c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1
:100644 100644 a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1 a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2 M\tsrc/a.rs
";

    const A1: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    const A2: &str = "a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2";
    const A3: &str = "a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3";
    const B1: &str = "b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1";
    const EDIT: &str = "e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0";

    /// needle-8879251b: the shape an `update-ref` advance leaves — index and
    /// worktree still at the older commit's content, a file the skipped
    /// commit added absent — is stale; an ordinary edit is not.
    #[test]
    fn a_tree_left_behind_by_a_ref_move_is_stale_and_an_edit_is_not() {
        let history = parse_log_raw(LOG);
        let stale = classify(
            &[
                // Index and worktree both at c1's/c2's content of a.rs.
                state("src/a.rs", Some(A3), Some(A2), Some(A2)),
                // b.rs was added by c2; the stale tree never had it.
                state("src/b.rs", Some(B1), None, None),
                // Somebody's in-flight edit: content no commit ever had.
                state("src/c.rs", Some(A1), Some(A1), Some(EDIT)),
            ],
            &history,
        );
        assert_eq!(stale.len(), 2, "{stale:?}");
        assert_eq!(stale[0].path, "src/a.rs");
        assert_eq!(stale[0].side, StaleSide::Both);
        assert_eq!(
            stale[0].matches_commit.as_deref(),
            Some("c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3^"),
            "a2 was replaced by c3, so it is the state just before c3"
        );
        assert_eq!(stale[1].path, "src/b.rs");
        assert_eq!(stale[1].side, StaleSide::Both);
        assert_eq!(
            stale[1].matches_commit.as_deref(),
            Some("c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2^"),
            "absent is the state just before the commit that added it"
        );

        // Worktree-only staleness (index already at HEAD) is still stale.
        let stale = classify(&[state("src/a.rs", Some(A3), Some(A3), Some(A1))], &history);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].side, StaleSide::Worktree);
        assert_eq!(
            stale[0].matches_commit.as_deref(),
            Some("c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1^")
        );

        // A file deleted on disk with the index still at HEAD is an ordinary
        // deletion, even though the path once did not exist.
        assert!(classify(&[state("src/b.rs", Some(B1), Some(B1), None)], &history).is_empty());

        // A path matching HEAD on both sides is never stale.
        assert!(classify(&[state("src/a.rs", Some(A3), Some(A3), Some(A3))], &history).is_empty());
    }

    #[test]
    fn git_record_parsers_read_index_tree_and_name_lists() {
        let index = parse_blob_records_z(&format!(
            "100644 {A2} 0\tsrc/a.rs\0100644 {B1} 2\tsrc/conflict.rs\0"
        ));
        assert_eq!(index.get("src/a.rs").map(String::as_str), Some(A2));
        assert!(
            !index.contains_key("src/conflict.rs"),
            "a conflicted stage is not a committed state"
        );

        let tree = parse_blob_records_z(&format!(
            "100644 blob {A3}\tsrc/a.rs\0160000 commit {B1}\tvendor/sub\0"
        ));
        assert_eq!(tree.get("src/a.rs").map(String::as_str), Some(A3));
        assert!(
            !tree.contains_key("vendor/sub"),
            "a submodule is not a blob"
        );

        assert_eq!(
            parse_names_z("src/a.rs\0.beads/issues.jsonl\0"),
            vec!["src/a.rs".to_string(), ".beads/issues.jsonl".to_string()]
        );
        assert!(is_bead_state(".beads/issues.jsonl"));
        assert!(!is_bead_state(".beadsy/x"));
    }

    fn drift_ctx(stale: usize) -> AuditContext {
        let mut context = AuditContext {
            root: std::path::PathBuf::from("/fixture-root"),
            config: crate::config::Config::default(),
            workspaces: Vec::new(),
            live_worker_identities: BTreeSet::new(),
            collected_at: chrono::Utc::now(),
            ledger: Vec::new(),
            ci: BTreeMap::new(),
            ci_templates: BTreeMap::new(),
            beads: BTreeMap::new(),
            attempts: BTreeMap::new(),
            tree_drift: BTreeMap::new(),
        };
        let paths = (0..stale)
            .map(|index| StalePath {
                path: format!("src/file_{index}.rs"),
                side: StaleSide::Both,
                matches_commit: Some("c1c1c1c1c1c1c1c1".to_string()),
            })
            .collect();
        context.tree_drift.insert(
            "NEEDLE".to_string(),
            Collected::Available(Some(TreeDrift {
                head: "c3c3c3c3c3c3c3c3c3c3".to_string(),
                differing: stale + 4,
                stale: paths,
            })),
        );
        context
    }

    /// The finding fires at `min_paths`, names the paths and the commit they
    /// match, is one signature per workspace, and files as an operational
    /// defect.
    #[test]
    fn f7_reports_a_drifted_checkout_at_the_path_threshold_only() {
        let min = crate::config::AuditTreeDriftConfig::default_min_paths() as usize;
        assert!(StaleTreeDrift
            .check(&drift_ctx(min - 1))
            .expect("predicate runs")
            .is_empty());

        let findings = StaleTreeDrift
            .check(&drift_ctx(85))
            .expect("predicate runs");
        assert_eq!(findings.len(), 1);
        let finding = &findings[0];
        assert_eq!(finding.rule, F7_STALE_TREE_DRIFT);
        assert_eq!(finding.scope, "NEEDLE");
        assert_eq!(finding.subject, SUBJECT);
        assert_eq!(finding.count, 85);
        assert!(finding
            .detail
            .contains("src/file_0.rs (index+worktree = c1c1c1c1c1c1)"));
        assert!(finding.detail.contains("(+65 more)"), "{}", finding.detail);
        assert!(finding.detail.contains("update-ref"));
        assert_eq!(short("c2c2c2c2c2c2c2c2c2c2^"), "c2c2c2c2c2c2^");
        assert!(crate::cli::audit::filing::is_operational_defect(finding));
        assert_eq!(crate::cli::audit::filing::priority_for(finding), Some(0));
        let labels = crate::cli::audit::filing::labels_for(finding);
        for expected in ["operational-defect", "escalation"] {
            assert!(labels.iter().any(|l| l == expected), "{labels:?}");
        }
        assert!(!labels
            .iter()
            .any(|l| l == "human" || l == "proposal-pending"));
    }
}
