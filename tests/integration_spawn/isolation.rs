//! Child-process isolation shared by the `integration_spawn` target.
//!
//! The fixture changes only a child's environment. Tests therefore remain
//! parallel-safe while every HOME-, XDG-, state-, discovery-, and temp-derived
//! path stays below one directory that is removed on drop.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};

use tempfile::TempDir;

/// One isolated filesystem namespace for commands spawned by a test.
pub struct IsolatedChildEnv {
    root: TempDir,
}

impl IsolatedChildEnv {
    /// Create the directory layout used by child commands.
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("create isolated child environment");
        for relative in [".cache", ".config", ".local/state", ".needle/state", "tmp"] {
            fs::create_dir_all(root.path().join(relative))
                .unwrap_or_else(|error| panic!("create isolated {relative}: {error}"));
        }
        Self { root }
    }

    /// The fixture root. It is also the child's HOME and workspace scan root.
    pub fn path(&self) -> &Path {
        self.root.path()
    }

    /// Construct the compiled NEEDLE binary with the isolated environment.
    pub fn needle(&self) -> Command {
        self.command(env!("CARGO_BIN_EXE_needle"))
    }

    /// Construct any child command with the same isolated environment.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(self.path())
            .env("HOME", self.path())
            .env("XDG_CONFIG_HOME", self.path().join(".config"))
            .env("XDG_STATE_HOME", self.path().join(".local/state"))
            .env("XDG_CACHE_HOME", self.path().join(".cache"))
            .env("XDG_RUNTIME_DIR", self.path().join(".local/state"))
            .env("TMPDIR", self.path().join("tmp"))
            .env("NEEDLE_HOME", self.path().join(".needle"))
            .env("NEEDLE_STATE_DIR", self.path().join(".needle/state"))
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
