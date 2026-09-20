//! End-to-end claim-verification coverage through the real `needle` binary.
//!
//! These tests deliberately use two independent bead-rs workspaces.  The
//! colliding case copies the same issue into both stores, closes the home copy,
//! and leaves the remote copy claimable.  A dispatcher that accidentally
//! verifies against its home store therefore aborts before the marker agent is
//! spawned.  The fixture adapter is intentionally boring: it records one line
//! per agent spawn and closes the claimed issue.
//!
//! Every `needle` child gets a private HOME.  Explore is enabled so the remote
//! routing path is real, but its workspace list remains empty and its scan root
//! is pinned to that same private HOME.  This is the subprocess form of the
//! ADR-006 isolation rule.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

const ADAPTER_NAME: &str = "claim-verification-probe";
const WORKER_NAME: &str = "claim-verification-worker";
const MARKER: &str = "needle-agent-spawned.log";
const QUERY_LOG: &str = ".needle-claim-query.log";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FixtureMode {
    Success,
    IssueNotFound,
    WrongBackendIdentity,
    MalformedJson,
    Timeout,
    UnavailableCli,
    StatusMismatch,
    AssigneeMismatch,
    RevisionMismatch,
    EpochMismatch,
    /// The agent spawns, records its identity, then exits 1 without closing
    /// the bead: a handled work failure that must release the claim.
    AgentFail,
    /// The agent spawns, records its identity, then outlives the adapter
    /// timeout: a dispatch timeout that must release the claim.
    AgentHang,
}

impl FixtureMode {
    fn env_name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::IssueNotFound => "issue-not-found",
            Self::WrongBackendIdentity => "wrong-backend-identity",
            Self::MalformedJson => "malformed-json",
            Self::Timeout => "timeout",
            Self::UnavailableCli => "unavailable-cli",
            Self::StatusMismatch => "status-mismatch",
            Self::AssigneeMismatch => "assignee-mismatch",
            Self::RevisionMismatch => "revision-mismatch",
            Self::EpochMismatch => "epoch-mismatch",
            Self::AgentFail => "agent-fail",
            Self::AgentHang => "agent-hang",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FixtureLayout {
    CollidingRemote,
    NonCollidingRemote,
    Local,
}

struct Fixture {
    root: TempDir,
    home_workspace: PathBuf,
    remote_workspace: Option<PathBuf>,
    home_bead_id: String,
    remote_bead_id: Option<String>,
    bead_binary: PathBuf,
}

impl Fixture {
    fn new(layout: FixtureLayout) -> Self {
        let root = tempfile::tempdir().expect("create subprocess fixture root");
        let home_workspace = root.path().join("home-workspace");
        fs::create_dir_all(&home_workspace).expect("create home workspace");

        let bead_binary = native_bead_binary();
        init_workspace(&bead_binary, &home_workspace, root.path(), "home");
        let home_id = create_bead(&bead_binary, &home_workspace, "home fixture bead", "4");

        let (remote_workspace, remote_bead_id) = match layout {
            FixtureLayout::Local => None,
            FixtureLayout::CollidingRemote => {
                let remote = root.path().join("remote-workspace");
                fs::create_dir_all(&remote).expect("create remote workspace");
                init_git_workspace(&remote);
                copy_tree(&home_workspace.join(".beads"), &remote.join(".beads"));
                Some((remote, home_id.clone()))
            }
            FixtureLayout::NonCollidingRemote => {
                let remote = root.path().join("remote-workspace");
                fs::create_dir_all(&remote).expect("create remote workspace");
                init_workspace(&bead_binary, &remote, root.path(), "remote");
                let remote_id = create_bead(&bead_binary, &remote, "remote fixture bead", "0");
                Some((remote, remote_id))
            }
        }
        .map_or((None, None), |(workspace, bead_id)| {
            (Some(workspace), Some(bead_id))
        });

        match layout {
            FixtureLayout::Local => {}
            FixtureLayout::CollidingRemote | FixtureLayout::NonCollidingRemote => {
                close_bead(
                    &bead_binary,
                    &home_workspace,
                    &home_id,
                    "home fixture is not the selected target",
                );
            }
        }

        let adapter_dir = root.path().join("adapters");
        fs::create_dir_all(&adapter_dir).expect("create adapter directory");
        write_adapter(&adapter_dir, &bead_binary);
        write_global_config(root.path(), &adapter_dir);
        write_workspace_config(&home_workspace, &bead_binary);
        if let Some(remote) = &remote_workspace {
            write_workspace_config(remote, &bead_binary);
        }

        Self {
            root,
            home_workspace,
            remote_workspace,
            home_bead_id: home_id,
            remote_bead_id,
            bead_binary,
        }
    }

    fn run(&self, mode: FixtureMode) -> Output {
        self.command(mode)
            .output()
            .expect("spawn isolated needle subprocess")
    }

    /// Build (and prepare the fixture for) one worker invocation. The
    /// preparation is idempotent, so several commands built from the same
    /// fixture race over the very same stores.
    fn command(&self, mode: FixtureMode) -> Command {
        let home_binary = self.root.path().join("home-fixture-bead");
        let remote_binary = self.root.path().join("remote-fixture-bead");
        write_bead_wrapper(&home_binary, &self.bead_binary, Some("success"));
        write_workspace_config(&self.home_workspace, &home_binary);
        if let Some(remote) = self.remote_workspace.as_ref() {
            write_bead_wrapper(&remote_binary, &self.bead_binary, None);
            write_workspace_config(remote, &remote_binary);
        } else {
            write_bead_wrapper(&remote_binary, &self.bead_binary, None);
        }

        let mut command = Command::new(env!("CARGO_BIN_EXE_needle"));
        command
            .current_dir(&self.home_workspace)
            .env("HOME", self.root.path())
            .env("NEEDLE_INNER", "1")
            .env("NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK", "1")
            .env("NEEDLE_STRANDS__EXPLORE__ENABLED", "true")
            .env("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT", self.root.path())
            .env("NEEDLE_FIXTURE_MODE", mode.env_name())
            .env("NEEDLE_FIXTURE_NATIVE_BEAD", &self.bead_binary)
            .env("NEEDLE_FIXTURE_WORKER", WORKER_NAME)
            .args([
                "run",
                "--workspace",
                self.home_workspace.to_str().expect("fixture path is UTF-8"),
                "--agent",
                ADAPTER_NAME,
                "--identifier",
                WORKER_NAME,
            ]);
        command
    }

    fn marker_lines(&self, workspace: &Path) -> usize {
        fs::read_to_string(workspace.join(MARKER))
            .map(|content| content.lines().count())
            .unwrap_or(0)
    }

    /// The attempt identity every spawned agent recorded from its own
    /// environment, in spawn order.
    fn marker_attempt_ids(&self, workspace: &Path) -> Vec<String> {
        fs::read_to_string(workspace.join(MARKER))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.strip_prefix("spawned ").map(str::to_string))
            .collect()
    }

    /// The prompt bytes the agent consumed on stdin.
    fn captured_prompt(&self, workspace: &Path) -> String {
        fs::read_to_string(workspace.join(".needle-attempt-prompt.txt")).unwrap_or_default()
    }

    /// Every `attempt.resolved` ledger row the runs emitted.
    fn resolved_rows(&self) -> Vec<Value> {
        self.telemetry()
            .into_iter()
            .filter(|event| event["event_type"] == "attempt.resolved")
            .collect()
    }

    fn show_counts(&self) -> String {
        let mut counts = vec![format!(
            "home={}",
            fs::read_to_string(self.home_workspace.join(".needle-claim-show-count"))
                .unwrap_or_else(|_| "0".to_string())
                .trim()
        )];
        if let Some(remote) = &self.remote_workspace {
            counts.push(format!(
                "remote={}",
                fs::read_to_string(remote.join(".needle-claim-show-count"))
                    .unwrap_or_else(|_| "0".to_string())
                    .trim()
            ));
        }
        counts.join(", ")
    }

    fn query_log(&self, workspace: &Path) -> Vec<String> {
        fs::read_to_string(workspace.join(QUERY_LOG))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn assert_remote_only_queried(&self, mode: FixtureMode) {
        let remote = self
            .remote_workspace
            .as_ref()
            .expect("remote fixture must have a selected target workspace");
        let home_queries = self.query_log(&self.home_workspace);
        let remote_queries = self.query_log(remote);
        assert!(
            home_queries.iter().all(|query| query == "--version"),
            "{mode:?}: worker home store must not perform a claim lookup; home={home_queries:?}, remote={remote_queries:?}"
        );
        assert!(
            remote_queries
                .iter()
                .any(|query| query == "--version" || query.starts_with("show ")),
            "{mode:?}: selected remote store must record the backend or claim lookup"
        );
    }

    fn raw_response(&self) -> String {
        self.remote_workspace
            .as_ref()
            .or(Some(&self.home_workspace))
            .and_then(|workspace| fs::read_to_string(workspace.join(".needle-claim-show-raw")).ok())
            .unwrap_or_else(|| "<no captured response>".to_string())
    }

    fn transformed_response(&self) -> String {
        self.remote_workspace
            .as_ref()
            .or(Some(&self.home_workspace))
            .and_then(|workspace| {
                fs::read_to_string(workspace.join(".needle-claim-show-transformed")).ok()
            })
            .unwrap_or_else(|| "<no captured response>".to_string())
    }

    fn telemetry(&self) -> Vec<Value> {
        let log_dir = self.root.path().join("logs");
        let mut events = Vec::new();
        let Ok(entries) = fs::read_dir(log_dir) else {
            return events;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(content) = fs::read_to_string(path) else {
                continue;
            };
            for line in content.lines().filter(|line| !line.trim().is_empty()) {
                events
                    .push(serde_json::from_str(line).expect("telemetry JSONL must be valid JSON"));
            }
        }
        events
    }

    fn has_event(&self, event_type: &str) -> bool {
        self.telemetry()
            .iter()
            .any(|event| event["event_type"] == event_type)
    }

    fn bead_record(&self, workspace: &Path, bead_id: &str) -> Value {
        let output = run_checked(
            bead_command(&self.bead_binary, workspace, self.root.path())
                .args(["show", bead_id, "--json"]),
            "read fixture bead state",
        );
        let records: Vec<Value> = serde_json::from_slice(&output.stdout)
            .expect("bead show --json must return a JSON array");
        records
            .into_iter()
            .next()
            .expect("bead show --json must return the requested bead")
    }

    fn assert_open_and_unassigned(&self, workspace: &Path, bead_id: &str) {
        let record = self.bead_record(workspace, bead_id);
        assert_eq!(record["status"], "open", "bead must be released for retry");
        assert_eq!(
            record["assignee"],
            Value::Null,
            "released bead must be unassigned"
        );
    }

    fn assert_claim_verification_error(&self, workspace: &Path) {
        let event = self
            .telemetry()
            .into_iter()
            .find(|event| event["event_type"] == "bead.claim.verify_error")
            .expect("failed claim verification must emit structured telemetry");
        assert_eq!(event["data"]["stage"], "dispatching");
        assert_eq!(
            event["data"]["target_workspace"],
            workspace.display().to_string(),
            "verification telemetry must identify the store that was queried"
        );
        assert_eq!(event["data"]["category"], "lookup");
        assert!(event["data"]["detail"].is_string());
    }
}

fn native_bead_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("BEAD_RS_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    if let Ok(path) = which::which("bead") {
        return path;
    }
    let fallback = PathBuf::from("/home/coding/.cargo/bin/bead");
    assert!(
        fallback.is_file(),
        "bead-rs CLI is required for this matrix"
    );
    fallback
}

fn bead_command(binary: &Path, workspace: &Path, home: &Path) -> Command {
    let mut command = Command::new(binary);
    command.current_dir(workspace).env("HOME", home);
    command
}

fn run_checked(command: &mut Command, description: &str) -> Output {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{description}: failed to spawn fixture command: {error}"));
    assert!(
        output.status.success(),
        "{description}: exit={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn init_workspace(binary: &Path, workspace: &Path, home: &Path, prefix: &str) {
    fs::create_dir_all(workspace).expect("create workspace");
    init_git_workspace(workspace);
    run_checked(
        bead_command(binary, workspace, home).args([
            "init",
            "--prefix",
            prefix,
            "--skip-foreign-workspace",
        ]),
        "initialize fixture bead store",
    );
}

fn init_git_workspace(workspace: &Path) {
    run_checked(
        Command::new("git")
            .current_dir(workspace)
            .args(["init", "-q"]),
        "initialize fixture git repository",
    );
}

fn create_bead(binary: &Path, workspace: &Path, title: &str, priority: &str) -> String {
    let output = run_checked(
        bead_command(binary, workspace, workspace).args([
            "create",
            "--title",
            title,
            "--priority",
            priority,
        ]),
        "create fixture bead",
    );
    String::from_utf8(output.stdout)
        .expect("fixture bead id is UTF-8")
        .trim()
        .to_string()
}

fn close_bead(binary: &Path, workspace: &Path, id: &str, reason: &str) {
    run_checked(
        bead_command(binary, workspace, workspace).args(["close", id, "--reason", reason]),
        "close home fixture bead",
    );
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create copied fixture directory");
    for entry in fs::read_dir(source).expect("read fixture tree") {
        let entry = entry.expect("read fixture tree entry");
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to)
                .unwrap_or_else(|error| panic!("copy fixture file {}: {error}", from.display()));
        }
    }
}

fn yaml_path(path: &Path) -> String {
    path.to_str().expect("fixture path is UTF-8").to_string()
}

fn write_workspace_config(workspace: &Path, bead_binary: &Path) {
    let config = format!(
        "bead_cli:\n  backend: bead-rs\n  path: {}\n",
        yaml_path(bead_binary)
    );
    fs::write(workspace.join(".needle.yaml"), config).expect("write fixture workspace config");
}

fn write_global_config(home: &Path, adapter_dir: &Path) {
    let config_dir = home.join(".config/needle");
    fs::create_dir_all(&config_dir).expect("create isolated global config directory");
    let config = format!(
        "agent:\n  default: {}\n  adapters_dir: {}\n  routing: null\nworker:\n  idle_action: exit\n  allow_exit_without_supervisor: true\n  enforce_shipped_work: false\n  cpu_load_warn: 1.0\n  memory_free_warn_mb: 1\nworkspace:\n  home: {}\nstrands:\n  explore:\n    enabled: true\n    workspace_root: {}\n    workspaces: []\n    scan_interval_cycles: 1\n    max_scan_interval_cycles: 1\n  pluck:\n    circuit_breaker:\n      enabled: false\n  mend:\n    enabled: false\n  mitosis:\n    enabled: false\n  weave:\n    enabled: false\n  unravel:\n    enabled: false\n  pulse:\n    enabled: false\n  reflect:\n    enabled: false\n  splice:\n    enabled: false\n  knot:\n    enabled: false\ntelemetry:\n  file_sink:\n    enabled: true\n    log_dir: {}/logs\n  stdout_sink:\n    enabled: false\n  otlp_sink:\n    enabled: false\n",
        ADAPTER_NAME,
        yaml_path(adapter_dir),
        yaml_path(&home.join(".needle")),
        yaml_path(home),
        yaml_path(home),
    );
    fs::write(config_dir.join("config.yaml"), config).expect("write isolated global config");
}

fn write_adapter(adapter_dir: &Path, bead_binary: &Path) {
    // The fixture agent records the attempt identity it was dispatched with
    // (`NEEDLE_ATTEMPT_ID` comes in through the child environment), captures
    // the prompt it was handed on stdin, and then either fails, hangs past
    // the adapter timeout, or delivers the external close — mode-dependent.
    let adapter = format!(
        "name: {ADAPTER_NAME}\nagent_cli: /bin/sh\ninvoke_template: >\n  cd {{workspace}} && echo \"spawned ${{NEEDLE_ATTEMPT_ID:-none}}\" >> {MARKER} && cat > .needle-attempt-prompt.txt && if [ \"${{NEEDLE_FIXTURE_MODE:-}}\" = \"agent-hang\" ]; then sleep 31; elif [ \"${{NEEDLE_FIXTURE_MODE:-}}\" = \"agent-fail\" ]; then exit 1; fi && {} close {{bead_id}} --reason 'fixture agent completed'\ntimeout_secs: 10\nprovider: local\nmodel: fixture\n",
        yaml_path(bead_binary)
    );
    fs::write(adapter_dir.join("claim-verification-probe.yaml"), adapter)
        .expect("write fixture adapter");
}

fn write_bead_wrapper(wrapper: &Path, native: &Path, mode_override: Option<&str>) {
    let script = r##"#!/bin/sh
set -eu

native="${NEEDLE_FIXTURE_NATIVE_BEAD}"
mode="__NEEDLE_FIXTURE_MODE__"

if [ "$mode" != "success" ] || [ "${1:-}" = "show" ]; then
  printf '%s\n' "$*" >> "$PWD/.needle-claim-query.log"
fi

case "${1:-}" in
  claim|update)
    : > "$PWD/.needle-claim-established"
    ;;
esac

if [ "$mode" = "unavailable-cli" ] && [ -f "$PWD/.needle-claim-established" ]; then
  printf '%s\n' 'fixture bead CLI unavailable' >&2
  exit 127
fi

if [ "${1:-}" = "--version" ] && [ "$mode" = "wrong-backend-identity" ] && [ -f "$PWD/.needle-claim-established" ]; then
  printf '%s\n' 'not-a-bead-cli 9.9.9'
  exit 0
fi

if [ "${1:-}" = "show" ] && [ "$mode" != "success" ] && [ "$mode" != "wrong-backend-identity" ] && [ "$mode" != "unavailable-cli" ] && [ "$mode" != "agent-fail" ] && [ "$mode" != "agent-hang" ]; then
  count_file="$PWD/.needle-claim-show-count"
  count=0
  if [ -f "$count_file" ]; then
    count=$(cat "$count_file")
  fi
  count=$((count + 1))
  printf '%s\n' "$count" > "$count_file"

  # Claim acquisition consumes several show calls before the dispatch gates.
  # Mutate every later read so whichever final verification stage is reached
  # observes the requested failure, while earlier claim establishment remains
  # valid.
  if [ "$count" -ge 8 ]; then
    case "$mode" in
      issue-not-found)
        printf '%s\n' 'issue not found' >&2
        exit 3
        ;;
      malformed-json)
        printf '%s\n' '{not-json'
        exit 0
        ;;
      timeout)
        sleep 31
        exit 0
        ;;
      status-mismatch)
        raw=$("$native" "$@")
        printf '%s\n' "$raw" > "$PWD/.needle-claim-show-raw"
        transformed=$(printf '%s\n' "$raw" | sed -E 's/"status"[[:space:]]*:[[:space:]]*"in_progress"/"status":"open"/')
        printf '%s\n' "$transformed" > "$PWD/.needle-claim-show-transformed"
        printf '%s\n' "$transformed"
        exit 0
        ;;
      assignee-mismatch)
        raw=$("$native" "$@")
        printf '%s\n' "$raw" > "$PWD/.needle-claim-show-raw"
        printf '%s\n' "$raw" | sed -E 's/"assignee"[[:space:]]*:[[:space:]]*"[^"]*"/"assignee":"foreign-worker"/'
        exit 0
        ;;
      revision-mismatch)
        raw=$("$native" "$@")
        printf '%s\n' "$raw" > "$PWD/.needle-claim-show-raw"
        printf '%s\n' "$raw" | sed -E 's/"revision"[[:space:]]*:[[:space:]]*[0-9]+/"revision":999999/'
        exit 0
        ;;
      epoch-mismatch)
        raw=$("$native" "$@")
        printf '%s\n' "$raw" > "$PWD/.needle-claim-show-raw"
        printf '%s\n' "$raw" | sed -E 's/"claim_epoch"[[:space:]]*:[[:space:]]*[0-9]+/"claim_epoch":999999/'
        exit 0
        ;;
      *)
        ;;
    esac
  fi
fi

exec "$native" "$@"
"##;
    fs::write(
        wrapper,
        script
            .replace(
                "${NEEDLE_FIXTURE_NATIVE_BEAD}",
                &native.display().to_string(),
            )
            .replace(
                "__NEEDLE_FIXTURE_MODE__",
                mode_override.unwrap_or("${NEEDLE_FIXTURE_MODE:-success}"),
            ),
    )
    .expect("write fixture bead wrapper");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(wrapper)
            .expect("stat fixture bead wrapper")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(wrapper, permissions).expect("make fixture bead wrapper executable");
    }
}

fn assert_no_agent_spawn(fixture: &Fixture) {
    assert_eq!(
        fixture.marker_lines(&fixture.home_workspace),
        0,
        "home workspace must not spawn an agent"
    );
    if let Some(remote) = &fixture.remote_workspace {
        assert_eq!(
            fixture.marker_lines(remote),
            0,
            "remote workspace must not spawn an agent"
        );
    }
}

#[test]
fn subprocess_claim_verification_routes_remote_collisions_and_local_work() {
    let colliding = Fixture::new(FixtureLayout::CollidingRemote);
    let output = colliding.run(FixtureMode::Success);
    assert!(
        output.status.success(),
        "colliding remote dispatch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let remote = colliding.remote_workspace.as_ref().expect("remote fixture");
    assert_eq!(
        colliding.remote_bead_id.as_deref(),
        Some(colliding.home_bead_id.as_str()),
        "the colliding fixture must expose the same bead ID in both stores"
    );
    assert_eq!(colliding.marker_lines(&colliding.home_workspace), 0);
    colliding.assert_remote_only_queried(FixtureMode::Success);
    assert_eq!(
        colliding.marker_lines(remote),
        1,
        "remote marker missing; status={:?}\nstdout={}\nstderr={}\nevents={:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        colliding.telemetry()
    );
    let successes: Vec<_> = colliding
        .telemetry()
        .into_iter()
        .filter(|event| event["event_type"] == "bead.claim.verify_success")
        .collect();
    assert_eq!(
        successes.len(),
        1,
        "successful remote dispatch must verify exactly once"
    );
    let success = successes
        .into_iter()
        .next()
        .expect("successful remote dispatch emits claim verification telemetry");
    assert_eq!(success["data"]["workspace"], remote.display().to_string());
    assert!(success["data"]["claim_epoch"].as_u64().is_some());

    let noncolliding = Fixture::new(FixtureLayout::NonCollidingRemote);
    let output = noncolliding.run(FixtureMode::Success);
    assert!(
        output.status.success(),
        "non-colliding remote dispatch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let remote = noncolliding
        .remote_workspace
        .as_ref()
        .expect("remote fixture");
    assert_ne!(
        noncolliding.remote_bead_id.as_deref(),
        Some(noncolliding.home_bead_id.as_str()),
        "the non-colliding fixture must use distinct bead IDs"
    );
    assert_eq!(noncolliding.marker_lines(&noncolliding.home_workspace), 0);
    noncolliding.assert_remote_only_queried(FixtureMode::Success);
    assert_eq!(noncolliding.marker_lines(remote), 1);

    let local = Fixture::new(FixtureLayout::Local);
    let output = local.run(FixtureMode::Success);
    assert!(
        output.status.success(),
        "local dispatch regression failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(local.marker_lines(&local.home_workspace), 1);
    let local_success = local
        .telemetry()
        .into_iter()
        .find(|event| {
            event["event_type"] == "bead.claim.verify_success"
                && event["data"]["workspace"] == local.home_workspace.display().to_string()
        })
        .expect("local dispatch must emit target workspace verification telemetry");
    assert!(local_success["data"]["claim_epoch"].as_u64().is_some());
}

#[test]
fn subprocess_unverifiable_cleanup_uses_held_target_store_for_local_and_remote() {
    let local = Fixture::new(FixtureLayout::Local);
    let output = local.run(FixtureMode::IssueNotFound);
    assert!(
        !output.status.success(),
        "local unverifiable claim must abort"
    );
    assert_no_agent_spawn(&local);
    local.assert_open_and_unassigned(&local.home_workspace, &local.home_bead_id);
    local.assert_claim_verification_error(&local.home_workspace);

    let remote = Fixture::new(FixtureLayout::CollidingRemote);
    let output = remote.run(FixtureMode::IssueNotFound);
    assert!(
        !output.status.success(),
        "remote unverifiable claim must abort"
    );
    assert_no_agent_spawn(&remote);
    remote.assert_remote_only_queried(FixtureMode::IssueNotFound);
    let remote_workspace = remote
        .remote_workspace
        .as_ref()
        .expect("colliding fixture must have a remote workspace");
    let remote_bead_id = remote
        .remote_bead_id
        .as_ref()
        .expect("colliding fixture must have a remote bead");
    remote.assert_open_and_unassigned(remote_workspace, remote_bead_id);

    // The same ID exists in the worker's home store, but cleanup must use the
    // held target-store handle and credential instead of touching that copy.
    let home_record = remote.bead_record(&remote.home_workspace, &remote.home_bead_id);
    assert_eq!(home_record["status"], "closed");
    assert_eq!(home_record["assignee"], Value::Null);
    remote.assert_claim_verification_error(remote_workspace);
}

#[test]
fn subprocess_claim_verification_failure_matrix_spawns_zero_agents() {
    assert_failure_matrix_spawns_zero_agents();
}

/// Run the failure fixtures from another p2 module as well.  Keeping the
/// fixture runner here makes the subprocess setup single-sourced while the
/// fail-closed acceptance filter can exercise the same real-binary matrix.
pub(super) fn assert_failure_matrix_spawns_zero_agents() {
    for layout in [
        FixtureLayout::CollidingRemote,
        FixtureLayout::NonCollidingRemote,
    ] {
        for mode in [
            FixtureMode::IssueNotFound,
            FixtureMode::WrongBackendIdentity,
            FixtureMode::MalformedJson,
            FixtureMode::Timeout,
            FixtureMode::UnavailableCli,
        ] {
            let fixture = Fixture::new(layout);
            let output = fixture.run(mode);
            assert!(
                !output.status.success(),
                "{layout:?}/{mode:?}: failure mode must fail the worker subprocess\nstdout={}\nstderr={}\nevents={:?}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                fixture.telemetry()
            );
            assert_no_agent_spawn(&fixture);
            fixture.assert_remote_only_queried(mode);
            if mode != FixtureMode::WrongBackendIdentity && mode != FixtureMode::UnavailableCli {
                assert!(
                    fixture.has_event("bead.claim.verify_error"),
                    "{layout:?}/{mode:?}: verification failure must emit bead.claim.verify_error\nstdout={}\nstderr={}\nevents={:?}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                    fixture.telemetry()
                );
            }
        }
    }
}

#[test]
fn subprocess_claim_verification_identity_mismatches_abort_before_spawn() {
    for mode in [
        FixtureMode::StatusMismatch,
        FixtureMode::AssigneeMismatch,
        FixtureMode::RevisionMismatch,
        FixtureMode::EpochMismatch,
    ] {
        let fixture = Fixture::new(FixtureLayout::CollidingRemote);
        let output = fixture.run(mode);
        assert!(
            !output.status.success(),
            "{mode:?}: identity mismatch must fail the worker subprocess\nshow_counts={}\nraw_response={}\ntransformed_response={}\nstdout={}\nstderr={}",
            fixture.show_counts(),
            fixture.raw_response(),
            fixture.transformed_response(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_no_agent_spawn(&fixture);
        let diagnostics = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            diagnostics.contains("claim verification failed"),
            "{mode:?}: stderr must identify a claim-verification failure, got:\n{}",
            diagnostics
        );
        assert!(
            fixture.has_event("bead.claim.recheck_failed"),
            "{mode:?}: identity mismatch must emit recheck failure telemetry"
        );
    }
}

#[test]
fn subprocess_attempt_identity_flows_from_claim_to_adapter_and_resolution() {
    let fixture = Fixture::new(FixtureLayout::Local);
    let output = fixture.run(FixtureMode::Success);
    assert!(
        output.status.success(),
        "identity-flow dispatch failed:\nstdout={}\nstderr={}\nevents={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fixture.telemetry()
    );

    // The adapter child saw exactly one attempt identity in its environment,
    // minted before the claim it dispatched under.
    let ids = fixture.marker_attempt_ids(&fixture.home_workspace);
    assert_eq!(ids.len(), 1, "exactly one agent spawn must be recorded");
    let attempt_id = &ids[0];
    let parsed = uuid::Uuid::parse_str(attempt_id)
        .unwrap_or_else(|error| panic!("marker attempt id {attempt_id} must be a UUID: {error}"));
    assert_eq!(parsed.get_version_num(), 7, "attempt ids are UUIDv7");

    // The prompt the adapter consumed carries the same identity tag.
    let prompt = fixture.captured_prompt(&fixture.home_workspace);
    assert!(
        prompt.contains(&format!("[needle-attempt:{attempt_id}]")),
        "prompt must carry the dispatch attempt tag; got:\n{}",
        prompt
    );

    // Every telemetry event stamped inside the dispatch cycle carries that
    // one identity — no second id may appear anywhere in the run.
    let events = fixture.telemetry();
    let stamped: Vec<&str> = events
        .iter()
        .filter_map(|event| event["attempt_id"].as_str())
        .collect();
    assert!(
        !stamped.is_empty(),
        "dispatch-cycle events must be stamped with the attempt id"
    );
    assert!(
        stamped.iter().all(|stamped| stamped == attempt_id),
        "one attempt, one id: stamped ids {stamped:?} != marker id {attempt_id}"
    );

    // The resolved ledger row joins the external deliverable (the agent's
    // own close) to the same identity, with the claim's provenance facts.
    let resolved: Vec<&Value> = events
        .iter()
        .filter(|event| event["event_type"] == "attempt.resolved")
        .collect();
    assert_eq!(resolved.len(), 1, "exactly one resolved ledger row");
    let row = &resolved[0]["data"];
    assert_eq!(row["attempt_id"], attempt_id.as_str());
    assert_eq!(row["bead_id"], fixture.home_bead_id);
    assert_eq!(row["adapter"], ADAPTER_NAME);
    assert_eq!(row["model"], "fixture");
    assert!(
        row["assignee"]
            .as_str()
            .unwrap_or_default()
            .contains(WORKER_NAME),
        "resolved row must name the claimant assignee: {row}"
    );
    assert!(
        row["claim_revision"].as_u64().is_some(),
        "starting bead revision must be captured: {row}"
    );
    assert!(
        row["claim_epoch"].as_u64().is_some(),
        "lease/fencing epoch must be captured: {row}"
    );
    assert!(
        row["context_manifest_hash"]
            .as_str()
            .unwrap_or_default()
            .starts_with("sha256:"),
        "context manifest hash must be captured: {row}"
    );
    assert_eq!(
        row["outcome"], "verified_success",
        "the agent's external close must be credited to this attempt: {row}"
    );
}

#[test]
fn subprocess_failed_attempt_releases_and_retry_mints_a_fresh_identity() {
    let fixture = Fixture::new(FixtureLayout::Local);

    let first = fixture.run(FixtureMode::AgentFail);
    assert!(
        first.status.success(),
        "a failed agent attempt is a handled outcome, not a worker crash:\nstdout={}\nstderr={}\nevents={:?}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr),
        fixture.telemetry()
    );
    let first_ids = fixture.marker_attempt_ids(&fixture.home_workspace);
    assert_eq!(first_ids.len(), 1);
    // Missing/failed propagation must not strand a claim: the bead is
    // released and immediately re-claimable.
    fixture.assert_open_and_unassigned(&fixture.home_workspace, &fixture.home_bead_id);
    let first_rows = fixture.resolved_rows();
    assert_eq!(
        first_rows.len(),
        1,
        "the failed attempt still resolves exactly once"
    );
    assert_eq!(first_rows[0]["data"]["attempt_id"], first_ids[0].as_str());
    assert_ne!(
        first_rows[0]["data"]["outcome"], "verified_success",
        "an agent that shipped nothing must not be credited: {first_rows:?}"
    );

    // The retry over the released bead mints a NEW identity end to end.
    let second = fixture.run(FixtureMode::Success);
    assert!(
        second.status.success(),
        "retry dispatch failed:\nstdout={}\nstderr={}\nevents={:?}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr),
        fixture.telemetry()
    );
    let ids = fixture.marker_attempt_ids(&fixture.home_workspace);
    assert_eq!(ids.len(), 2, "the retry spawned exactly one more agent");
    assert_ne!(
        ids[1], ids[0],
        "a retry must not reuse the failed attempt's identity"
    );
    let rows = fixture.resolved_rows();
    assert_eq!(rows.len(), 2, "each attempt owns exactly one ledger row");
    let retry_row = rows
        .iter()
        .find(|row| row["data"]["attempt_id"] == ids[1].as_str())
        .expect("retry attempt must resolve under its own fresh identity");
    assert_eq!(retry_row["data"]["outcome"], "verified_success");
}

#[test]
fn subprocess_agent_timeout_carries_the_attempt_identity_and_releases() {
    let fixture = Fixture::new(FixtureLayout::Local);
    let output = fixture.run(FixtureMode::AgentHang);
    assert!(
        output.status.success(),
        "an agent timeout is a handled outcome, not a worker crash:\nstdout={}\nstderr={}\nevents={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fixture.telemetry()
    );

    // The timed-out agent still spawned under a recorded identity.
    let ids = fixture.marker_attempt_ids(&fixture.home_workspace);
    assert_eq!(ids.len(), 1);
    let attempt_id = &ids[0];
    assert_ne!(
        attempt_id, "none",
        "the timed-out agent must still receive NEEDLE_ATTEMPT_ID"
    );

    // The timeout releases the claim — no stranded bead — and the resolved
    // row joins the timeout to the same identity without crediting it.
    fixture.assert_open_and_unassigned(&fixture.home_workspace, &fixture.home_bead_id);
    let rows = fixture.resolved_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["data"]["attempt_id"], attempt_id.as_str());
    assert_ne!(
        rows[0]["data"]["outcome"], "verified_success",
        "a timed-out attempt must not be credited: {:?}",
        rows[0]
    );
}

#[test]
fn subprocess_concurrent_claim_race_mints_distinct_attempt_ids() {
    let fixture = Fixture::new(FixtureLayout::Local);

    // Two workers race for the single bead. Each mints its attempt identity
    // BEFORE the claim mutation, so even the loser's identity is observable.
    let first = fixture
        .command(FixtureMode::Success)
        .spawn()
        .expect("spawn first racing worker");
    let second = fixture
        .command(FixtureMode::Success)
        .spawn()
        .expect("spawn second racing worker");
    let out_a = first.wait_with_output().expect("wait first racing worker");
    let out_b = second
        .wait_with_output()
        .expect("wait second racing worker");
    for (name, output) in [("first", &out_a), ("second", &out_b)] {
        assert!(
            output.status.success(),
            "{name} racing worker failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // The atomic backend claim let exactly one worker dispatch an agent.
    let ids = fixture.marker_attempt_ids(&fixture.home_workspace);
    assert_eq!(ids.len(), 1, "a claim race must yield exactly one dispatch");

    // Both workers minted before claiming: two distinct identities in
    // telemetry, only the winner's reached an agent.
    let telemetry = fixture.telemetry();
    let stamped: std::collections::HashSet<&str> = telemetry
        .iter()
        .filter_map(|event| event["attempt_id"].as_str())
        .collect();
    assert!(
        stamped.contains(ids[0].as_str()),
        "the winner's identity must be stamped on its events: {stamped:?}"
    );
    assert!(
        stamped.len() >= 2,
        "the race loser minted its own identity before claiming: {stamped:?}"
    );

    // The winner's external deliverable closed the bead under its identity.
    let record = fixture.bead_record(&fixture.home_workspace, &fixture.home_bead_id);
    assert_eq!(record["status"], "closed");
}
