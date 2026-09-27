//! End-to-end regression coverage for ADR-003 cleanup liveness.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tempfile::TempDir;

struct CleanupFixture {
    root: TempDir,
    workspace: PathBuf,
    state: PathBuf,
    socket: String,
    binary: OsString,
    real_home: OsString,
}

impl CleanupFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("create cleanup fixture root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        fs::create_dir_all(&workspace).expect("create fixture workspace");
        fs::create_dir_all(&state).expect("create fixture state root");
        fs::write(
            workspace.join(".needle.yaml"),
            "bead_cli:\n  backend: bead-rs\n  path: /home/coding/.cargo/bin/bead\n",
        )
        .expect("write fixture backend binding");

        let config_dir = root.path().join(".config/needle");
        fs::create_dir_all(&config_dir).expect("create fixture config directory");
        fs::write(
            config_dir.join("config.yaml"),
            format!(
                "workspace:\n  default: {}\n  home: {}\nworker:\n  memory_free_warn_mb: 9000000000\n",
                workspace.display(),
                state.display(),
            ),
        )
        .expect("write fixture config");

        let socket = format!("needle-cleanup-{}-{}", std::process::id(), unique_suffix());
        let real_home = std::env::var_os("HOME").unwrap_or_default();
        let binary = std::env::var_os("NEXTEST_BIN_EXE_needle")
            .unwrap_or_else(|| OsString::from(env!("CARGO_BIN_EXE_needle")));

        Self {
            root,
            workspace,
            state,
            socket,
            binary,
            real_home,
        }
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.workspace)
            .env("HOME", self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join(".config"))
            .env("XDG_STATE_HOME", self.root.path().join(".local/state"))
            .env("XDG_CACHE_HOME", self.root.path().join(".cache"))
            .env("XDG_RUNTIME_DIR", self.root.path().join(".local/state"))
            .env("TMPDIR", self.root.path().join("tmp"))
            .env("NEEDLE_HOME", &self.state)
            .env("NEEDLE_STATE_DIR", &self.state)
            .env("NEEDLE_TEST_HARNESS", &self.real_home)
            .env("NEEDLE_TMUX_SOCKET", &self.socket)
            .env("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT", &self.workspace)
            .env_remove("NEEDLE_STRANDS__EXPLORE__WORKSPACES")
            .env_remove("NEEDLE_INNER")
            .env_remove("NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK")
            .env("NEEDLE_ADMISSION_BACKOFF_BASE_MS", "10")
            .env("NEEDLE_ADMISSION_BACKOFF_CAP_MS", "10")
            .env("NEEDLE_ADMISSION_HEARTBEAT_SECS", "1");
        command
    }

    fn needle(&self) -> Command {
        self.command(&self.binary)
    }

    fn tmux(&self) -> Command {
        let mut command = Command::new("tmux");
        command.args(["-L", &self.socket]);
        command
    }

    fn new_session(&self, name: &str, shell_command: &str) {
        let output = self
            .tmux()
            .args(["new-session", "-d", "-s", name, shell_command])
            .output()
            .expect("launch isolated tmux session");
        assert!(
            output.status.success(),
            "tmux session launch failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn session_exists(&self, name: &str) -> bool {
        self.tmux()
            .args(["has-session", "-t", name])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn pane_pid(&self, name: &str) -> u32 {
        let output = self
            .tmux()
            .args(["list-panes", "-t", name, "-F", "#{pane_pid}"])
            .output()
            .expect("read tmux pane PID");
        assert!(output.status.success(), "tmux pane PID lookup failed");
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("tmux pane PID is numeric")
    }

    fn wait_for_needle_pid(&self, session: &str) -> u32 {
        let pane_pid = self.pane_pid(session);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let children = process_children();
            let mut pending = vec![pane_pid];
            let mut visited = HashSet::new();
            while let Some(pid) = pending.pop() {
                if !visited.insert(pid) {
                    continue;
                }
                if is_needle_run_process(pid) {
                    return pid;
                }
                if let Some(descendants) = children.get(&pid) {
                    pending.extend(descendants);
                }
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("needle worker did not start below tmux pane {pane_pid}");
    }

    fn register_worker(&self, worker_id: &str, pid: u32) {
        let registry = self.state.join("state/workers.json");
        fs::create_dir_all(registry.parent().expect("registry has parent"))
            .expect("create fixture registry directory");
        let entry = serde_json::json!({
            "workers": [{
                "id": worker_id,
                "pid": pid,
                "workspace": self.workspace,
                "agent": "claude",
                "model": null,
                "provider": null,
                "started_at": "2026-01-01T00:00:00Z",
                "beads_processed": 0,
                "beads_completed": 0,
                "config_reload_generation": 0,
                "state": null
            }],
            "updated_at": "2026-01-01T00:00:00Z"
        });
        fs::write(
            registry,
            serde_json::to_vec_pretty(&entry).expect("serialize fixture registry"),
        )
        .expect("write fixture registry");
    }

    fn cleanup(&self, args: &[&str]) -> String {
        let output = self
            .needle()
            .args(args)
            .output()
            .expect("run needle cleanup");
        assert!(
            output.status.success(),
            "needle cleanup failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
}

impl Drop for CleanupFixture {
    fn drop(&mut self) {
        let _ = self
            .tmux()
            .args(["kill-server"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn process_children() -> HashMap<u32, Vec<u32>> {
    let mut children = HashMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return children;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(status) = fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        let Some(parent) = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:\t"))
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        children.entry(parent).or_insert_with(Vec::new).push(pid);
    }
    children
}

fn is_needle_run_process(pid: u32) -> bool {
    let Ok(cmdline) = fs::read(Path::new("/proc").join(pid.to_string()).join("cmdline")) else {
        return false;
    };
    let args = cmdline
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect::<Vec<_>>();
    let binary_index = usize::from(args.first().is_some_and(|arg| arg == "NEEDLE_INNER=1"));
    let Some(binary) = args.get(binary_index) else {
        return false;
    };
    let Some(command) = args.get(binary_index + 1) else {
        return false;
    };
    let Some(binary_name) = Path::new(binary).file_name() else {
        return false;
    };
    matches!(
        binary_name.to_string_lossy().as_ref(),
        "needle" | "needle-stable" | "needle-stable.prev" | "needle-testing"
    ) && command == "run"
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_nanos()
}

#[test]
fn cleanup_liveness_preserves_live_removes_orphan_and_all_overrides() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping cleanup liveness test: tmux is unavailable");
        return;
    }

    let fixture = CleanupFixture::new();
    let live_session = "needle-claude-live";
    let orphan_session = "needle-claude-orphan";
    let live_log = fixture.state.join("live.stderr.log");

    let live_command = format!(
        "NEEDLE_INNER=1 {} run --workspace {} --identifier live 2>> {}",
        shell_quote(Path::new(&fixture.binary)),
        shell_quote(&fixture.workspace),
        shell_quote(&live_log),
    );
    fixture.new_session(live_session, &live_command);
    let live_pid = fixture.wait_for_needle_pid(live_session);
    fixture.register_worker("claude-live", live_pid);

    // This session has a live pane process, but no live NEEDLE process or
    // registered worker behind it, so bare cleanup must classify it as orphaned.
    fixture.new_session(orphan_session, "sleep 3600");

    let output = fixture.cleanup(&["cleanup"]);
    assert!(
        fixture.session_exists(live_session),
        "bare cleanup must preserve the registered live worker: {output}"
    );
    assert!(
        !fixture.session_exists(orphan_session),
        "bare cleanup must remove the orphaned session: {output}"
    );

    // --all is intentionally destructive and must still remove the live
    // session, preserving its explicit override semantics.
    let output = fixture.cleanup(&["cleanup", "--all"]);
    assert!(
        !fixture.session_exists(live_session),
        "cleanup --all must remove live sessions too: {output}"
    );
}
