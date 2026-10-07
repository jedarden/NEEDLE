//! Keep file-contention markers out of Git (plan Phase 20.1, needle-9e778fad).
//!
//! The marker tree is ephemeral, checkout-local runtime state. It must never
//! be committed, pushed, checkpointed, captured in a patch, uploaded as
//! evidence, or treated as a project artifact. Three layers:
//!
//! 1. **Prevention** — [`ensure_local_exclude`] adds `/.needle/locks/` to the
//!    repository's local `.git/info/exclude` (no committed `.gitignore`
//!    change is required), so `git add -A` / `git add .` never stage it.
//! 2. **Collection** — every NEEDLE path that lists or captures changed files
//!    applies [`EXCLUDE_PATHSPEC`] or [`is_marker_path`], so a marker never
//!    appears as work, dirt, evidence, or patch content.
//! 3. **Rejection** — [`guard_no_markers`] fails when a marker is staged or
//!    tracked, including one added with `git add -f`; [`markers_in_range`]
//!    finds markers already inside commits so NEEDLE refuses to amend or
//!    accept them.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Line added to `.git/info/exclude`.
pub const EXCLUDE_ENTRY: &str = "/.needle/locks/";
/// Repository-relative prefix of every marker path.
pub const MARKER_PREFIX: &str = ".needle/locks/";
/// Git pathspec that removes the marker tree from a command's scope. Use it
/// as `["--", ".", EXCLUDE_PATHSPEC]` after the other arguments.
pub const EXCLUDE_PATHSPEC: &str = ":(exclude).needle/locks";

/// True for the marker tree itself or anything inside it.
pub fn is_marker_path(path: &str) -> bool {
    let path = path.trim_start_matches("./");
    path == ".needle/locks" || path.starts_with(MARKER_PREFIX)
}

/// Drop marker paths from a list of repository-relative paths.
pub fn retain_non_markers(paths: &mut Vec<String>) {
    paths.retain(|p| !is_marker_path(p));
}

/// Make sure the repository's LOCAL exclude file ignores the marker tree.
/// Idempotent; returns `true` when the entry was added. Works for linked
/// worktrees, whose `info/exclude` lives in the common git directory.
pub fn ensure_local_exclude(repo_root: &Path) -> Result<bool> {
    let exclude = local_exclude_path(repo_root)?;
    let existing = match std::fs::read_to_string(&exclude) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("read {}", exclude.display())),
    };
    if existing.lines().any(|line| line.trim() == EXCLUDE_ENTRY) {
        return Ok(false);
    }
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(
        "# NEEDLE file-contention markers: checkout-local runtime state (plan Phase 20)\n",
    );
    text.push_str(EXCLUDE_ENTRY);
    text.push('\n');
    std::fs::write(&exclude, text).with_context(|| format!("write {}", exclude.display()))?;
    Ok(true)
}

/// Marker paths that are staged or tracked (which includes `git add -f`).
pub fn staged_or_tracked_markers(repo_root: &Path) -> Result<Vec<String>> {
    let mut found = git_paths(
        repo_root,
        &[
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--",
            ".needle/locks",
        ],
    )?;
    found.extend(git_paths(
        repo_root,
        &["ls-files", "-z", "--", ".needle/locks"],
    )?);
    found.retain(|p| is_marker_path(p));
    found.sort();
    found.dedup();
    Ok(found)
}

/// Fail when any marker is staged or tracked.
pub fn guard_no_markers(repo_root: &Path) -> Result<()> {
    let found = staged_or_tracked_markers(repo_root)?;
    if !found.is_empty() {
        bail!(
            "file-contention markers must never be committed; staged or tracked: {}. \
             Unstage with `git rm --cached -r -- .needle/locks`",
            found.join(", ")
        );
    }
    Ok(())
}

/// Marker paths touched by commits in `base..head`.
pub fn markers_in_range(repo_root: &Path, base: &str, head: &str) -> Result<Vec<String>> {
    let range = format!("{base}..{head}");
    let mut found = git_paths(
        repo_root,
        &[
            "log",
            "--format=",
            "--name-only",
            "-z",
            &range,
            "--",
            ".needle/locks",
        ],
    )?;
    found.retain(|p| is_marker_path(p));
    found.sort();
    found.dedup();
    Ok(found)
}

fn local_exclude_path(repo_root: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .output()
        .context("run git rev-parse --git-path info/exclude")?;
    if !output.status.success() {
        bail!(
            "{} is not a git repository: {}",
            repo_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let raw = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    Ok(if raw.is_absolute() {
        raw
    } else {
        repo_root.join(raw)
    })
}

fn git_paths(repo_root: &Path, args: &[&str]) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        // -C names the repository; an inherited hook environment must not
        // redirect the query to another index.
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output
        .stdout
        .split(|b| *b == 0 || *b == b'\n')
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect())
}

/// Real-git fixtures shared by every file-contention unit test, so the
/// subprocess stays inside this module's single owned `process` exception.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;
    use std::process::Command;

    /// Run git in `repo` with a fixture identity and no inherited repo env.
    pub(crate) fn git(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// An initialised repository with one commit of `src/lib.rs`.
    pub(crate) fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn one() {}\n").unwrap();
        git(dir.path(), &["add", "--", "src/lib.rs"]);
        git(dir.path(), &["commit", "-q", "-m", "initial"]);
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_contention::store::{
        MarkerRead, MarkerStore, Participant, PathIntent, WriteIntent,
    };
    use std::time::Duration;

    use super::test_support::git;

    /// A repo with one commit and an active marker for src/lib.rs.
    fn repo_with_marker() -> tempfile::TempDir {
        let dir = super::test_support::init_repo();
        let store = MarkerStore::open(dir.path()).unwrap();
        let who = Participant {
            bead_id: Some("needle-1".into()),
            attempt_id: Some("a1".into()),
            session_id: None,
            worker_id: "w1".into(),
            host: "codinghome".into(),
            pid: 1,
        };
        let never = |_: &str, _: &MarkerRead| false;
        store
            .acquire(
                &who,
                &[PathIntent {
                    path: "src/lib.rs".into(),
                    intent: WriteIntent::Modify,
                }],
                Duration::from_secs(60),
                chrono::Utc::now(),
                &never,
            )
            .unwrap();
        dir
    }

    #[test]
    fn marker_path_predicate_and_filter() {
        assert!(is_marker_path(".needle/locks"));
        assert!(is_marker_path(".needle/locks/src/lib.rs.lock"));
        assert!(is_marker_path("./.needle/locks/.registry.lock"));
        assert!(!is_marker_path(".needle/config.yaml"));
        assert!(!is_marker_path("src/.needle/locks/x"));
        let mut paths = vec![
            "src/lib.rs".to_string(),
            ".needle/locks/src/lib.rs.lock".to_string(),
        ];
        retain_non_markers(&mut paths);
        assert_eq!(paths, vec!["src/lib.rs"]);
    }

    #[test]
    fn local_exclude_is_installed_once_without_touching_gitignore() {
        let dir = repo_with_marker();
        assert!(ensure_local_exclude(dir.path()).unwrap());
        assert!(!ensure_local_exclude(dir.path()).unwrap(), "idempotent");
        let exclude = std::fs::read_to_string(dir.path().join(".git/info/exclude")).unwrap();
        assert_eq!(exclude.matches(EXCLUDE_ENTRY).count(), 1);
        assert!(
            !dir.path().join(".gitignore").exists(),
            "no committed .gitignore change"
        );
        // Ordinary status and add-all no longer see the marker tree.
        assert!(!git(
            dir.path(),
            &["status", "--porcelain", "--untracked-files=all"]
        )
        .contains(".needle"));
        git(dir.path(), &["add", "-A"]);
        assert!(staged_or_tracked_markers(dir.path()).unwrap().is_empty());
        guard_no_markers(dir.path()).unwrap();
    }

    #[test]
    fn exclude_pathspec_hides_markers_even_without_the_exclude_entry() {
        let dir = repo_with_marker();
        let unfiltered = git(
            dir.path(),
            &["status", "--porcelain", "--untracked-files=all"],
        );
        assert!(
            unfiltered.contains(".needle/"),
            "precondition: marker visible without exclude"
        );
        let filtered = git(
            dir.path(),
            &[
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--",
                ".",
                EXCLUDE_PATHSPEC,
            ],
        );
        assert!(!filtered.contains(".needle"), "{filtered}");
    }

    #[test]
    fn force_added_marker_is_rejected() {
        let dir = repo_with_marker();
        ensure_local_exclude(dir.path()).unwrap();
        git(
            dir.path(),
            &["add", "-f", "--", ".needle/locks/src/lib.rs.lock"],
        );
        let found = staged_or_tracked_markers(dir.path()).unwrap();
        assert_eq!(found, vec![".needle/locks/src/lib.rs.lock"]);
        let err = guard_no_markers(dir.path()).unwrap_err().to_string();
        assert!(err.contains("must never be committed"), "{err}");
    }

    #[test]
    fn tracked_marker_in_history_is_found_by_range() {
        let dir = repo_with_marker();
        let base = git(dir.path(), &["rev-parse", "HEAD"]).trim().to_string();
        git(
            dir.path(),
            &["add", "-f", "--", ".needle/locks/src/lib.rs.lock"],
        );
        git(dir.path(), &["commit", "-q", "-m", "oops"]);
        assert_eq!(
            markers_in_range(dir.path(), &base, "HEAD").unwrap(),
            vec![".needle/locks/src/lib.rs.lock"]
        );
        assert!(
            guard_no_markers(dir.path()).is_err(),
            "a tracked marker is rejected too"
        );
    }

    #[test]
    fn linked_worktree_uses_the_common_exclude_file() {
        let dir = repo_with_marker();
        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("linked");
        git(
            dir.path(),
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                wt_path.to_str().unwrap(),
            ],
        );
        assert!(ensure_local_exclude(&wt_path).unwrap());
        let common = std::fs::read_to_string(dir.path().join(".git/info/exclude")).unwrap();
        assert!(common.contains(EXCLUDE_ENTRY));
    }
}
