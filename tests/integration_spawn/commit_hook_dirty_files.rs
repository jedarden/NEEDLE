//! Acceptance fixtures for the predispatch dirty-file commit guard.

use chrono::Utc;
use needle::commit_hook::validate_commit;
use needle::types::BeadId;
use needle::validation::predispatch::{self, DirtyFile, PreDispatch};
use serial_test::serial;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

struct EnvironmentGuard {
    home: Option<OsString>,
    state_dir: Option<OsString>,
}

impl EnvironmentGuard {
    fn set(home: &Path, state_dir: &Path) -> Self {
        let previous = Self {
            home: std::env::var_os("HOME"),
            state_dir: std::env::var_os(needle::state_dir::STATE_DIR_ENV),
        };
        std::env::set_var("HOME", home);
        std::env::set_var(needle::state_dir::STATE_DIR_ENV, state_dir);
        previous
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        match self.home.take() {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match self.state_dir.take() {
            Some(value) => std::env::set_var(needle::state_dir::STATE_DIR_ENV, value),
            None => std::env::remove_var(needle::state_dir::STATE_DIR_ENV),
        }
    }
}

struct GitFixture {
    _root: TempDir,
}

impl GitFixture {
    fn new() -> Self {
        let root = TempDir::new().expect("create git fixture");
        git_ok(root.path(), &["init", "-q"]);
        git_ok(root.path(), &["config", "user.name", "Needle Test"]);
        git_ok(
            root.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        fs::write(root.path().join("README.md"), "seed\n").expect("write seed");
        git_ok(root.path(), &["add", "README.md"]);
        git_ok(root.path(), &["commit", "-q", "-m", "seed"]);
        Self { _root: root }
    }

    fn path(&self) -> &Path {
        self._root.path()
    }
}

fn git_output(path: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("run git fixture command")
}

fn git_ok(path: &Path, args: &[&str]) {
    let output = git_output(path, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(path: &Path, args: &[&str]) -> String {
    let output = git_output(path, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git fixture output is UTF-8")
        .trim()
        .to_string()
}

fn isolated_environment() -> (TempDir, TempDir, EnvironmentGuard) {
    let home = TempDir::new().expect("create isolated HOME");
    let state = TempDir::new().expect("create isolated state root");
    let guard = EnvironmentGuard::set(home.path(), state.path());
    (home, state, guard)
}

fn write_snapshot(repo: &GitFixture, bead_id: &BeadId, dirty_files: Vec<DirtyFile>) {
    let snapshot = PreDispatch {
        head_sha: Some(git_stdout(repo.path(), &["rev-parse", "HEAD"])),
        notes_hash: None,
        dirty_files,
        captured_at: Some(Utc::now()),
    };
    let path = predispatch::snapshot_path(repo.path(), bead_id);
    fs::create_dir_all(path.parent().expect("snapshot parent")).expect("create snapshot parent");
    fs::write(
        path,
        serde_json::to_vec(&snapshot).expect("serialize snapshot"),
    )
    .expect("write snapshot");
}

#[serial]
#[tokio::test]
async fn foreign_dirty_file_edited_by_agent_is_allowed() {
    let (_home, _state, _environment) = isolated_environment();
    let repo = GitFixture::new();
    let bead_id = BeadId::from("needle-edited-dirty");
    let path = repo.path().join("shared.txt");

    fs::write(&path, "another worker\n").expect("write foreign edit");
    let blob_hash = git_stdout(repo.path(), &["hash-object", "--", "shared.txt"]);
    write_snapshot(
        &repo,
        &bead_id,
        vec![DirtyFile {
            path: "shared.txt".to_string(),
            blob_hash,
        }],
    );
    fs::write(&path, "this agent\n").expect("write agent edit");
    git_ok(repo.path(), &["add", "shared.txt"]);

    validate_commit(repo.path(), &bead_id)
        .await
        .expect("a changed staged blob is this agent's work");
}

#[serial]
#[tokio::test]
async fn clean_file_is_allowed() {
    let (_home, _state, _environment) = isolated_environment();
    let repo = GitFixture::new();
    let bead_id = BeadId::from("needle-clean-file");
    write_snapshot(&repo, &bead_id, Vec::new());

    fs::write(repo.path().join("new-work.txt"), "agent work\n").expect("write clean-file work");
    git_ok(repo.path(), &["add", "new-work.txt"]);

    validate_commit(repo.path(), &bead_id)
        .await
        .expect("a file clean at dispatch is allowed");
}
