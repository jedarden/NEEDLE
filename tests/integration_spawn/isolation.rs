//! Child-process isolation shared by the `integration_spawn` target.
//!
//! The fixture changes only a child's environment. Tests therefore remain
//! parallel-safe while every HOME-, XDG-, state-, discovery-, and temp-derived
//! path stays below one directory that is removed on drop.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};

use tempfile::TempDir;

/// Resolve a compiled binary at runtime so archived nextest runs remain
/// relocatable. Cargo's compile-time path points at the archive producer's
/// target directory, while nextest remaps this variable to each extraction
/// directory used by the runner.
pub fn needle_binary_path() -> std::ffi::OsString {
    std::env::var_os("NEXTEST_BIN_EXE_needle")
        .unwrap_or_else(|| std::ffi::OsString::from(env!("CARGO_BIN_EXE_needle")))
}

/// Runtime-relocatable path for the Claude event transformer binary.
pub fn needle_transform_claude_binary_path() -> std::ffi::OsString {
    std::env::var_os("NEXTEST_BIN_EXE_needle_transform_claude")
        .unwrap_or_else(|| std::ffi::OsString::from(env!("CARGO_BIN_EXE_needle-transform-claude")))
}

/// One isolated filesystem namespace for commands spawned by a test.
pub struct IsolatedChildEnv {
    root: TempDir,
    /// The home this process was launched with, captured before any child
    /// override. The harness guard carries it so a spawned binary can refuse
    /// a state root that resolves beneath the real home (N-T52).
    real_home: OsString,
}

impl IsolatedChildEnv {
    /// Create the directory layout used by child commands.
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("create isolated child environment");
        for relative in [".cache", ".config", ".local/state", ".needle/state", "tmp"] {
            fs::create_dir_all(root.path().join(relative))
                .unwrap_or_else(|error| panic!("create isolated {relative}: {error}"));
        }
        Self {
            root,
            real_home: std::env::var_os("HOME").unwrap_or_default(),
        }
    }

    /// The fixture root. It is also the child's HOME and workspace scan root.
    pub fn path(&self) -> &Path {
        self.root.path()
    }

    /// Construct the compiled NEEDLE binary with the isolated environment.
    pub fn needle(&self) -> Command {
        self.command(needle_binary_path())
    }

    /// Construct any child command with the same isolated environment.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(self.path())
            .env("HOME", self.path())
            .env("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT", self.path())
            // An inherited explicit list wins over workspace_root. Remove it
            // so a caller cannot accidentally re-enable discovery of a real
            // workspace through the parent test process's environment.
            .env_remove("NEEDLE_STRANDS__EXPLORE__WORKSPACES")
            .env("XDG_CONFIG_HOME", self.path().join(".config"))
            .env("XDG_STATE_HOME", self.path().join(".local/state"))
            .env("XDG_CACHE_HOME", self.path().join(".cache"))
            .env("XDG_RUNTIME_DIR", self.path().join(".local/state"))
            .env("TMPDIR", self.path().join("tmp"))
            .env("NEEDLE_HOME", self.path().join(".needle"))
            .env("NEEDLE_STATE_DIR", self.path().join(".needle/state"))
            // ADR-030 decision 5 (N-T52): the spawned binary refuses to run
            // under a test harness without an isolated state root, and
            // refuses one beneath the real home captured here.
            .env("NEEDLE_TEST_HARNESS", &self.real_home)
            .env(
                "NEEDLE_EVENTS",
                self.path().join(".needle/state/events.jsonl"),
            )
            .env(
                "NEEDLE_HEARTBEATS",
                self.path().join(".needle/state/heartbeats.jsonl"),
            )
            .env_remove("NEEDLE_INNER")
            .env_remove("NEEDLE_SUPERVISOR_SOCKET")
            .env_remove("NEEDLE_TMUX_SOCKET");
        command
    }
}

/// A spawned child that is killed and reaped if a test exits early or panics.
pub struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    pub fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    pub fn id(&self) -> u32 {
        self.child.as_ref().expect("child has not been reaped").id()
    }

    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.child
            .take()
            .expect("child has not already been reaped")
            .wait()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn isolated_child_pins_home_and_explore_root_to_fixture() {
    let fixture = IsolatedChildEnv::new();
    let outside = tempfile::tempdir().expect("create outside workspace fixture");
    std::fs::create_dir_all(outside.path().join(".beads"))
        .expect("create outside workspace marker");
    std::fs::write(outside.path().join("marker"), "must remain untouched")
        .expect("write outside workspace marker");

    let output = fixture
        .needle()
        .args(["config", "--dump"])
        .output()
        .expect("run isolated needle config dump");
    assert!(
        output.status.success(),
        "isolated config dump failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let config = String::from_utf8_lossy(&output.stdout);
    assert!(
        config.contains(&fixture.path().display().to_string()),
        "effective config must use the fixture as Explore root: {config}"
    );
    assert!(
        !config.contains(&outside.path().display().to_string()),
        "effective config must not discover an outside workspace: {config}"
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("marker"))
            .expect("read outside workspace marker"),
        "must remain untouched"
    );
}
