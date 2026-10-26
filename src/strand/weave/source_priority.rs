//! Ranked candidate sources a Weave pass may propose from.
//!
//! needle-21c5e0a3: a repository may legitimately have no more work to do.
//! Weave's priority goes to (1) recent code changes that leave an unaddressed
//! defect, then (2) gaps between `docs/plan/plan.md` and what is built. Of
//! 6,937 weave proposals measured, 2,373 test/coverage and 721 documentation
//! proposals cited neither a defect nor a plan gap, and 1,151 were
//! duplicates. This module supplies the two sources in rank order; when
//! neither exists the pass has nothing admissible to propose from and the
//! strand skips the model call entirely — emitting nothing is a valid result
//! and better than synthesising filler.
//!
//! Like [`super::candidate_filter`], everything here is a pure function of
//! the workspace, so the same inputs always give the same inventory.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use chrono::{Duration, Utc};

/// The conventional plan file (the repo-init convention in `CLAUDE.md`).
/// A citation of this file is a plan gap, not the generic documentation
/// citation [`super::candidate_filter`] rejects.
pub(crate) const PLAN_FILE: &str = "docs/plan/plan.md";

/// How far back a commit still counts as a recent code change.
const RECENT_CHANGE_LOOKBACK_DAYS: i64 = 14;
/// Upper bound on commits scanned for recent changes.
const RECENT_CHANGE_MAX_COMMITS: usize = 20;

/// Repository paths that churn independently of product code. `.beads/` in
/// particular is written by every bead mutation, so without the exclusion it
/// would dominate every recent-change list.
const CHANGE_PATH_EXCLUSIONS: &[&str] = &[".beads/", ".git/", ".needle/"];

/// The ranked sources a workspace offers a Weave pass.
#[derive(Debug, Clone, Default)]
pub(crate) struct SourceInventory {
    /// Rank 1: files touched by commits inside the lookback window,
    /// workspace-relative with forward slashes.
    recent_changes: BTreeSet<String>,
    /// Rank 2: the plan file exists.
    plan: bool,
}

impl SourceInventory {
    /// Discover the sources for `workspace`.
    pub(crate) fn discover(workspace: &Path) -> Self {
        Self {
            recent_changes: recent_changed_files(workspace),
            plan: workspace.join(PLAN_FILE).is_file(),
        }
    }

    /// Whether the pass has nothing admissible to propose from.
    pub(crate) fn is_empty(&self) -> bool {
        self.recent_changes.is_empty() && !self.plan
    }

    /// Rank-1 source: the recently changed files.
    pub(crate) fn recent_changes(&self) -> &BTreeSet<String> {
        &self.recent_changes
    }

    /// Whether the workspace has a plan file for rank-2 sourcing.
    pub(crate) fn has_plan(&self) -> bool {
        self.plan
    }

    /// Whether a cited workspace-relative path was touched by a recent
    /// commit, i.e. the citation is tied to a dated change.
    pub(crate) fn contains_recent_change(&self, relative: &str) -> bool {
        self.recent_changes.contains(relative)
    }
}

/// Whether a cited (workspace-relative) path is the plan file.
pub(crate) fn is_plan_file(relative: &str) -> bool {
    relative == PLAN_FILE
}

/// Files touched by commits inside the lookback window.
///
/// Any git failure — no repository, no commits yet, git not installed —
/// simply means "no recent changes": a quiet workspace is the normal case,
/// not an error.
fn recent_changed_files(workspace: &Path) -> BTreeSet<String> {
    let since = (Utc::now() - Duration::days(RECENT_CHANGE_LOOKBACK_DAYS)).format("%Y-%m-%d");
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args([
            "log",
            "--name-only",
            "--pretty=format:",
            "--since",
            &since.to_string(),
            &format!("-{RECENT_CHANGE_MAX_COMMITS}"),
        ])
        .output()
    else {
        return BTreeSet::new();
    };
    if !output.status.success() {
        return BTreeSet::new();
    }
    parse_changed_files(&String::from_utf8_lossy(&output.stdout))
}

fn parse_changed_files(output: &str) -> BTreeSet<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            !CHANGE_PATH_EXCLUSIONS
                .iter()
                .any(|excluded| line.starts_with(excluded))
        })
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
impl SourceInventory {
    pub(crate) fn fixture(recent_changes: &[&str]) -> Self {
        Self {
            recent_changes: recent_changes
                .iter()
                .map(|path| (*path).to_string())
                .collect(),
            plan: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_git_and_no_plan_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let inventory = SourceInventory::discover(dir.path());
        assert!(inventory.is_empty());
        assert!(!inventory.has_plan());
    }

    #[test]
    fn recent_changes_are_rank_one_and_ignore_harness_paths() {
        let changed = parse_changed_files(
            ".beads/checkpoint/current.json\n.needle/logs/telemetry.jsonl\nsrc/lib.rs\n",
        );
        assert_eq!(changed.len(), 1);
        let inventory = SourceInventory::fixture(&["src/lib.rs"]);
        assert!(!inventory.is_empty());
        assert!(inventory.contains_recent_change("src/lib.rs"));
        assert!(!inventory.contains_recent_change("src/other.rs"));
    }

    #[test]
    fn only_the_conventional_plan_path_is_rank_two() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs/plan")).unwrap();
        std::fs::write(dir.path().join("docs/plan/other.md"), "# Other plan\n").unwrap();
        assert!(SourceInventory::discover(dir.path()).is_empty());
        std::fs::write(dir.path().join(PLAN_FILE), "# Plan\n").unwrap();
        let inventory = SourceInventory::discover(dir.path());
        assert!(!inventory.is_empty());
        assert!(inventory.has_plan());
        assert!(is_plan_file("docs/plan/plan.md"));
        for path in ["docs/plan/PLAN.md", "docs/plan.md", "README.md"] {
            assert!(!is_plan_file(path), "{path}");
        }
    }
}
