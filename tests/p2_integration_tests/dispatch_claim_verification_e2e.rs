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
        let missing_binary = self.root.path().join("missing-bead-cli");
        let config_binary = if mode == FixtureMode::UnavailableCli {
            missing_binary.clone()
        } else {
            self.root.path().join("fixture-bead")
        };

        if mode != FixtureMode::UnavailableCli {
            write_bead_wrapper(&config_binary, &self.bead_binary);
            write_workspace_config(&self.home_workspace, &config_binary);
            if let Some(remote) = &self.remote_workspace {
                write_workspace_config(remote, &config_binary);
            }
        } else {
            write_workspace_config(&self.home_workspace, &missing_binary);
            if let Some(remote) = &self.remote_workspace {
                write_workspace_config(remote, &missing_binary);
            }
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
        command.output().expect("spawn isolated needle subprocess")
    }

    fn marker_lines(&self, workspace: &Path) -> usize {
        fs::read_to_string(workspace.join(MARKER))
            .map(|content| content.lines().count())
            .unwrap_or(0)
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
    let adapter = format!(
        "name: {ADAPTER_NAME}\nagent_cli: /bin/sh\ninvoke_template: >\n  cd {{workspace}} && echo spawned >> {MARKER} && {} close {{bead_id}} --reason 'fixture agent completed'\ntimeout_secs: 10\nprovider: local\nmodel: fixture\n",
        yaml_path(bead_binary)
    );
    fs::write(adapter_dir.join("claim-verification-probe.yaml"), adapter)
        .expect("write fixture adapter");
}

fn write_bead_wrapper(wrapper: &Path, native: &Path) {
    let script = r##"#!/bin/sh
set -eu

native="${NEEDLE_FIXTURE_NATIVE_BEAD}"
mode="${NEEDLE_FIXTURE_MODE:-success}"

if [ "${1:-}" = "--version" ] && [ "$mode" = "wrong-backend-identity" ]; then
  printf '%s\n' 'not-a-bead-cli 9.9.9'
  exit 0
fi

if [ "${1:-}" = "show" ] && [ "$mode" != "success" ] && [ "$mode" != "wrong-backend-identity" ] && [ "$mode" != "unavailable-cli" ]; then
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
        script.replace(
            "${NEEDLE_FIXTURE_NATIVE_BEAD}",
            &native.display().to_string(),
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
    assert_eq!(noncolliding.marker_lines(remote), 1);

    let local = Fixture::new(FixtureLayout::Local);
    let output = local.run(FixtureMode::Success);
    assert!(
        output.status.success(),
        "local dispatch regression failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(local.marker_lines(&local.home_workspace), 1);
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
