//! Integration coverage for the gate-less workspace fallback verifier.
//!
//! These tests exercise the public outcome-handler boundary, rather than the
//! detector's private implementation seam. Each fixture is a real local Git
//! repository, and the language tools are temporary shell shims that record
//! their invocations and exit deterministically. This keeps the tests
//! hermetic: no registry, package index, network, or operator HOME is used.

#[path = "gate_workspace_resolution.rs"]
mod gate_workspace_resolution;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;

use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::claim::{ClaimIdentity, ResolvedStoreContext};
use needle::config::Config;
use needle::dispatch::{AgentAdapter, DispatchContext, Dispatcher, TokenExtraction};
use needle::outcome::OutcomeHandler;
use needle::prompt::BuiltPrompt;
use needle::telemetry::Telemetry;
use needle::types::{
    AgentOutcome, Bead, BeadAction, BeadId, BeadStatus, ClaimResult, ClaimStatus, InputMethod,
    ReleaseReason,
};
use needle::validation::fallback::{select_verifier, MarkerVerifier, Verifier};

const WORKER: &str = "fallback-gate-integration";

// Environment variables are process-global. Serialize these tests with one
// another while they install a temporary HOME/PATH, and restore every value on
// drop even if the test panics.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct IsolatedEnvironment {
    _lock: MutexGuard<'static, ()>,
    home: tempfile::TempDir,
    previous_home: Option<OsString>,
    previous_path: Option<OsString>,
    previous_cargo_home: Option<OsString>,
    previous_cargo_target_dir: Option<OsString>,
    previous_cargo_net_offline: Option<OsString>,
    previous_goflags: Option<OsString>,
    previous_explore_workspace_root: Option<OsString>,
}

impl IsolatedEnvironment {
    fn new(with_tool_shims: bool) -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("isolated HOME");
        let previous_home = std::env::var_os("HOME");
        let previous_path = std::env::var_os("PATH");
        let previous_cargo_home = std::env::var_os("CARGO_HOME");
        let previous_cargo_target_dir = std::env::var_os("CARGO_TARGET_DIR");
        let previous_cargo_net_offline = std::env::var_os("CARGO_NET_OFFLINE");
        let previous_goflags = std::env::var_os("GOFLAGS");
        let previous_explore_workspace_root =
            std::env::var_os("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT");

        std::env::set_var("HOME", home.path());
        std::env::set_var("CARGO_HOME", home.path().join("cargo-home"));
        std::env::set_var("CARGO_TARGET_DIR", home.path().join("cargo-target"));
        std::env::set_var("CARGO_NET_OFFLINE", "true");
        std::env::set_var("GOFLAGS", "-trimpath");
        std::env::set_var(
            "NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT",
            home.path().join("explore-root"),
        );

        if with_tool_shims {
            let shim_dir = home.path().join("tool-shims");
            std::fs::create_dir_all(&shim_dir).expect("tool shim directory");
            let original_path = previous_path
                .as_deref()
                .unwrap_or_else(|| std::ffi::OsStr::new("/usr/bin:/bin"));
            let path = std::env::join_paths(
                std::iter::once(shim_dir.into_os_string())
                    .chain(std::env::split_paths(original_path).map(PathBuf::into_os_string)),
            )
            .expect("temporary PATH");
            std::env::set_var("PATH", path);
        }

        Self {
            _lock: lock,
            home,
            previous_home,
            previous_path,
            previous_cargo_home,
            previous_cargo_target_dir,
            previous_cargo_net_offline,
            previous_goflags,
            previous_explore_workspace_root,
        }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn shim_dir(&self) -> PathBuf {
        self.home.path().join("tool-shims")
    }
}

impl Drop for IsolatedEnvironment {
    fn drop(&mut self) {
        restore_env("HOME", self.previous_home.take());
        restore_env("PATH", self.previous_path.take());
        restore_env("CARGO_HOME", self.previous_cargo_home.take());
        restore_env("CARGO_TARGET_DIR", self.previous_cargo_target_dir.take());
        restore_env("CARGO_NET_OFFLINE", self.previous_cargo_net_offline.take());
        restore_env("GOFLAGS", self.previous_goflags.take());
        restore_env(
            "NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT",
            self.previous_explore_workspace_root.take(),
        );
    }
}

fn restore_env(name: &str, value: Option<OsString>) {
    match value {
        Some(value) => std::env::set_var(name, value),
        None => std::env::remove_var(name),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StoreAction {
    Release,
    Reopen,
    AddLabel(String),
    Flush,
}

struct OutcomeStore {
    bead: Bead,
    actions: Mutex<Vec<StoreAction>>,
    labels: Mutex<Vec<String>>,
}

impl OutcomeStore {
    fn new(bead: Bead) -> Self {
        Self {
            labels: Mutex::new(bead.labels.clone()),
            bead,
            actions: Mutex::new(Vec::new()),
        }
    }

    fn actions(&self) -> Vec<StoreAction> {
        self.actions.lock().unwrap().clone()
    }
}

#[async_trait]
impl BeadStore for OutcomeStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        Ok(Vec::new())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(vec![self.bead.clone()])
    }

    async fn show(&self, _id: &BeadId) -> Result<Bead> {
        let mut bead = self.bead.clone();
        bead.status = BeadStatus::Done;
        bead.labels = self.labels.lock().unwrap().clone();
        Ok(bead)
    }

    async fn claim_status(&self, _id: &BeadId) -> Result<ClaimStatus> {
        Ok(ClaimStatus {
            status: BeadStatus::InProgress,
            assignee: Some(WORKER.to_string()),
            revision: Some(1),
            claim_epoch: Some(1),
        })
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
        anyhow::bail!("not used by fallback-gate tests")
    }

    async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
        anyhow::bail!("not used by fallback-gate tests")
    }

    async fn release(&self, _id: &BeadId) -> Result<()> {
        self.actions.lock().unwrap().push(StoreAction::Release);
        Ok(())
    }

    async fn block(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        self.actions.lock().unwrap().push(StoreAction::Flush);
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> Result<()> {
        self.actions.lock().unwrap().push(StoreAction::Reopen);
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
        Ok(self.labels.lock().unwrap().clone())
    }

    async fn add_label(&self, _id: &BeadId, label: &str) -> Result<()> {
        self.labels.lock().unwrap().push(label.to_string());
        self.actions
            .lock()
            .unwrap()
            .push(StoreAction::AddLabel(label.to_string()));
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, label: &str) -> Result<()> {
        self.labels
            .lock()
            .unwrap()
            .retain(|current| current != label);
        Ok(())
    }

    async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
        Ok(BeadId::from("needle-fallback-repair"))
    }

    async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn remove_dependency(&self, _blocked_id: &BeadId, _blocker_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        Ok(RepairReport::default())
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        Ok(RepairReport::default())
    }

    async fn full_rebuild(&self) -> Result<()> {
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

fn bead(workspace: &Path, id: &str) -> Bead {
    Bead {
        id: BeadId::from(id),
        title: "fallback gate fixture".to_string(),
        body: Some("fixture".to_string()),
        priority: 1,
        status: BeadStatus::InProgress,
        assignee: Some(WORKER.to_string()),
        labels: Vec::new(),
        workspace: workspace.to_path_buf(),
        dependencies: Vec::new(),
        dependents: Vec::new(),
        comments: Vec::new(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn config_without_default_gates() -> Config {
    config_with_fallback_settings(30, 4096)
}

fn config_with_default_gates() -> Config {
    let mut config = Config::default();
    config.worker.enforce_shipped_work = false;
    config.validation.outcome_timeout_seconds = 30;
    config.validation.stderr_cap_bytes = 4096;
    config
}

fn config_with_fallback_settings(timeout_seconds: u64, stderr_cap_bytes: usize) -> Config {
    let mut config = Config::default();
    config.worker.enforce_shipped_work = false;
    config.validation.default_gates.enabled = false;
    config.validation.outcome_timeout_seconds = timeout_seconds;
    config.validation.stderr_cap_bytes = stderr_cap_bytes;
    config
}

fn write_file(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("fixture parent directory");
    }
    std::fs::write(path, contents).expect("fixture file");
}

fn shell_quote(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
}

fn install_tool_shim(environment: &IsolatedEnvironment, name: &str, log: &Path, exit: i32) {
    let script = format!(
        "#!/bin/sh\nprintf '{} %s|HOME=%s|PWD=%s\\n' \"$*\" \"$HOME\" \"$PWD\" >> {}\nexit {}\n",
        name,
        shell_quote(log),
        exit
    );
    let path = environment.shim_dir().join(name);
    write_file(&environment.shim_dir(), name, &script);
    let mut permissions = std::fs::metadata(&path)
        .expect("shim metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("shim executable");
    }
}

fn install_failing_tool_shim(
    environment: &IsolatedEnvironment,
    name: &str,
    stderr: &str,
    exit: i32,
) {
    let script = format!(
        "#!/bin/sh\nprintf '%s' '{}' >&2\nexit {}\n",
        stderr.replace('\'', "'\\''"),
        exit
    );
    let path = environment.shim_dir().join(name);
    write_file(&environment.shim_dir(), name, &script);
    let mut permissions = std::fs::metadata(&path)
        .expect("failing shim metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("failing shim executable");
    }
}

fn install_sleeping_tool_shim(environment: &IsolatedEnvironment, name: &str) {
    let path = environment.shim_dir().join(name);
    write_file(&environment.shim_dir(), name, "#!/bin/sh\nexec sleep 30\n");
    let mut permissions = std::fs::metadata(&path)
        .expect("sleeping shim metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("sleeping shim executable");
    }
}

fn commit_fixture(root: &Path, files: &[&str]) {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "user.email=needle@example.test"])
            .args(["-c", "user.name=needle-fallback-test"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .expect("git fixture command")
    };
    assert!(git(&["init", "-q"]).status.success());
    let mut add_args = vec!["add"];
    add_args.extend(files.iter().copied());
    assert!(git(&add_args).status.success());
    assert!(git(&["commit", "-q", "-m", "fixture"]).status.success());
}

fn marker_fixture(marker: &str, log: &Path) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("marker fixture");
    match marker {
        "go" => write_file(root.path(), "go.mod", "module example.test/fallback\n"),
        "rust" => write_file(
            root.path(),
            "Cargo.toml",
            "[package]\nname = \"fallback-fixture\"\nversion = \"0.1.0\"\n",
        ),
        "node" => write_file(
            root.path(),
            "package.json",
            r#"{"name":"fallback-fixture","scripts":{"test":"node test.js"}}"#,
        ),
        "python-pyproject" => write_file(
            root.path(),
            "pyproject.toml",
            "[project]\nname = 'fallback-fixture'\n",
        ),
        "python-pytest-ini" => write_file(root.path(), "pytest.ini", "[pytest]\n"),
        "definition-of-done" => {
            write_file(
                root.path(),
                "go.mod",
                "module example.test/definition-wins\n",
            );
            write_file(
                root.path(),
                "scripts/definition-of-done.sh",
                &format!(
                    "#!/bin/sh\nprintf '%s|HOME=%s|PWD=%s\\n' 'definition-of-done' \"$HOME\" \"$PWD\" >> {}\n",
                    shell_quote(log),
                ),
            );
            let script = root.path().join("scripts/definition-of-done.sh");
            let mut permissions = std::fs::metadata(&script)
                .expect("definition script metadata")
                .permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(0o755);
                std::fs::set_permissions(script, permissions)
                    .expect("definition script executable");
            }
        }
        other => panic!("unknown marker fixture {other}"),
    }

    let files = match marker {
        "go" => vec!["go.mod"],
        "rust" => vec!["Cargo.toml"],
        "node" => vec!["package.json"],
        "python-pyproject" => vec!["pyproject.toml"],
        "python-pytest-ini" => vec!["pytest.ini"],
        "definition-of-done" => vec!["go.mod", "scripts/definition-of-done.sh"],
        _ => unreachable!(),
    };
    commit_fixture(root.path(), &files);
    root
}

fn go_module_fixture(compiles: bool) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("Go module fixture");
    write_file(
        root.path(),
        "go.mod",
        "module example.test/clean-go-gate\n\ngo 1.20\n",
    );
    write_file(
        root.path(),
        "main.go",
        if compiles {
            "package main\n\nfunc main() {}\n"
        } else {
            "package main\n\nfunc main() { var _ int = \"compile failure\" }\n"
        },
    );
    write_file(
        root.path(),
        "clean_extraction_test.go",
        "package main\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\nfunc TestCleanExtractionHasNoGit(t *testing.T) {\n\tif _, err := os.Stat(\".git\"); !os.IsNotExist(err) {\n\t\tt.Fatalf(\"clean extraction contains .git: %v\", err)\n\t}\n}\n",
    );
    commit_fixture(
        root.path(),
        &["go.mod", "main.go", "clean_extraction_test.go"],
    );
    root
}

async fn run_real_go_fixture(
    config: Config,
    workspace: &Path,
    id: &str,
) -> (needle::types::HandlerResult, Vec<StoreAction>, String) {
    let store = OutcomeStore::new(bead(workspace, id));
    let log_dir = tempfile::tempdir().expect("Go gate telemetry directory");
    let telemetry = Telemetry::with_log_dir(WORKER.to_string(), log_dir.path());
    telemetry.start();
    let handler = OutcomeHandler::new(config, telemetry.clone());
    let result = handler
        .handle(
            &store,
            &bead(workspace, id),
            &AgentOutcome {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
            false,
        )
        .await
        .expect("Go gate handler");
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .expect("Go gate telemetry flush");
    let events = read_telemetry(log_dir.path());
    let actions = store.actions();
    telemetry.shutdown().await;
    (result, actions, events)
}

async fn run_fixture(
    environment: &IsolatedEnvironment,
    marker: &str,
    log: &Path,
) -> (needle::types::HandlerResult, Vec<StoreAction>, String) {
    for tool in ["go", "cargo", "npm", "pytest"] {
        install_tool_shim(environment, tool, log, 0);
    }
    let workspace = marker_fixture(marker, log);
    let store = OutcomeStore::new(bead(workspace.path(), marker));
    let log_dir = tempfile::tempdir().expect("telemetry log directory");
    let telemetry = Telemetry::with_log_dir(WORKER.to_string(), log_dir.path());
    telemetry.start();
    let handler = OutcomeHandler::new(config_without_default_gates(), telemetry.clone());
    let result = handler
        .handle(
            &store,
            &bead(workspace.path(), marker),
            &AgentOutcome {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
            false,
        )
        .await
        .expect("fallback handler");
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .expect("telemetry flush");
    let log_text = std::fs::read_to_string(log).unwrap_or_default();
    let actions = store.actions();
    telemetry.shutdown().await;
    (result, actions, log_text)
}

#[test]
fn fallback_marker_selection_covers_all_language_branches_and_fallthrough() {
    let cases = [
        (
            "go",
            vec![("go.mod", "module example.test/fallback\n")],
            Verifier::Marker(MarkerVerifier {
                language: "go",
                evidence: "go.mod",
                command: "go build ./... && go vet ./... && go test -short ./...",
            }),
        ),
        (
            "rust",
            vec![("Cargo.toml", "[package]\n")],
            Verifier::Marker(MarkerVerifier {
                language: "rust",
                evidence: "Cargo.toml",
                command: "cargo build --all-targets && cargo test",
            }),
        ),
        (
            "node",
            vec![("package.json", r#"{"scripts":{"test":"node test.js"}}"#)],
            Verifier::Marker(MarkerVerifier {
                language: "node",
                evidence: "package.json",
                command: "npm test",
            }),
        ),
        (
            "python-pyproject",
            vec![("pyproject.toml", "[project]\n")],
            Verifier::Marker(MarkerVerifier {
                language: "python",
                evidence: "pyproject.toml",
                command: "pytest -q",
            }),
        ),
        (
            "python-pytest-ini",
            vec![("pytest.ini", "[pytest]\n")],
            Verifier::Marker(MarkerVerifier {
                language: "python",
                evidence: "pytest.ini",
                command: "pytest -q",
            }),
        ),
    ];

    for (name, files, expected) in cases {
        let root = tempfile::tempdir().expect("selection fixture");
        for (relative, contents) in files {
            write_file(root.path(), relative, contents);
        }
        assert_eq!(select_verifier(root.path()), expected, "{name}");
    }

    let root = tempfile::tempdir().expect("package fallthrough fixture");
    write_file(
        root.path(),
        "package.json",
        r#"{"name":"fixture","scripts":{"lint":"eslint ."}}"#,
    );
    write_file(
        root.path(),
        "pyproject.toml",
        "[project]\nname = 'fixture'\n",
    );
    assert_eq!(
        select_verifier(root.path()),
        Verifier::Marker(MarkerVerifier {
            language: "python",
            evidence: "pyproject.toml",
            command: "pytest -q",
        })
    );

    let root = tempfile::tempdir().expect("package no-test fixture");
    write_file(root.path(), "package.json", r#"{"name":"fixture"}"#);
    assert_eq!(select_verifier(root.path()), Verifier::NoVerifier);
}

#[tokio::test(flavor = "current_thread")]
async fn builtin_go_gates_pass_in_gitless_clean_extractions() {
    let _environment = IsolatedEnvironment::new(false);

    for (id, config, expected_gate) in [
        (
            "default-go-clean",
            config_with_default_gates(),
            "default_go",
        ),
        (
            "fallback-go-clean",
            config_without_default_gates(),
            "fallback_go",
        ),
    ] {
        let workspace = go_module_fixture(true);
        let (result, actions, events) = run_real_go_fixture(config, workspace.path(), id).await;
        assert_eq!(result.outcome, needle::types::Outcome::Success, "{id}");
        assert_eq!(result.bead_action, BeadAction::Closed, "{id}");
        assert!(actions.contains(&StoreAction::Flush), "{id}: {actions:?}");
        assert!(
            events.contains(expected_gate),
            "{id} should report {expected_gate}: {events}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn builtin_go_gates_reject_a_deliberate_compile_error() {
    let _environment = IsolatedEnvironment::new(false);

    for (id, config, expected_gate) in [
        (
            "default-go-broken",
            config_with_default_gates(),
            "default_go",
        ),
        (
            "fallback-go-broken",
            config_without_default_gates(),
            "fallback_go",
        ),
    ] {
        let workspace = go_module_fixture(false);
        let (result, actions, events) = run_real_go_fixture(config, workspace.path(), id).await;
        assert_eq!(result.outcome, needle::types::Outcome::Failure, "{id}");
        assert_eq!(
            result.bead_action,
            BeadAction::Released(ReleaseReason::GateFailed),
            "{id}"
        );
        assert!(actions.contains(&StoreAction::Reopen), "{id}: {actions:?}");
        assert!(
            actions.contains(&StoreAction::AddLabel("verification-failed".into())),
            "{id}: {actions:?}"
        );
        assert!(
            events.contains(expected_gate),
            "{id} should report {expected_gate}: {events}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn fallback_marker_commands_are_selected_and_executed_in_clean_extractions() {
    let environment = IsolatedEnvironment::new(true);
    let cases = [
        (
            "go",
            ["go build ./...", "go vet ./...", "go test -short ./..."].as_slice(),
        ),
        (
            "rust",
            ["cargo build --all-targets", "cargo test"].as_slice(),
        ),
        ("node", ["npm test"].as_slice()),
        ("python-pyproject", ["pytest -q"].as_slice()),
        ("python-pytest-ini", ["pytest -q"].as_slice()),
        ("definition-of-done", ["definition-of-done"].as_slice()),
    ];

    for (marker, expected) in cases {
        let log = environment.home().join(format!("{marker}.log"));
        let (result, actions, log_text) = run_fixture(&environment, marker, &log).await;
        assert_eq!(result.outcome, needle::types::Outcome::Success, "{marker}");
        assert_eq!(result.bead_action, BeadAction::Closed, "{marker}");
        assert!(
            actions.contains(&StoreAction::Flush),
            "{marker}: {actions:?}"
        );
        for invocation in expected {
            assert!(
                log_text.lines().any(|line| line.starts_with(invocation)),
                "{marker} did not execute {invocation:?}; log={log_text:?}"
            );
        }
        assert!(
            log_text.contains(&format!("HOME={}", environment.home().display())),
            "{marker} did not receive isolated HOME; log={log_text:?}"
        );
        if marker == "definition-of-done" {
            assert!(
                !log_text.lines().any(|line| line.starts_with("go|")),
                "definition-of-done must win over go.mod: {log_text:?}"
            );
        }
    }

    assert_fallback_failed_verifier_captures_and_caps_stderr(&environment).await;
    assert_fallback_verifier_timeout_is_released_as_execution_error(&environment).await;
    gate_workspace_resolution::run_gate_workspace_resolution_regression()
        .await
        .expect("per-workspace gate resolution regression");
}

async fn assert_fallback_failed_verifier_captures_and_caps_stderr(
    environment: &IsolatedEnvironment,
) {
    let log = environment.home().join("stderr.log");
    install_failing_tool_shim(environment, "go", "0123456789abcdef", 23);
    let workspace = marker_fixture("go", &log);
    let store = OutcomeStore::new(bead(workspace.path(), "stderr-failure"));
    let log_dir = tempfile::tempdir().expect("stderr telemetry directory");
    let telemetry = Telemetry::with_log_dir(WORKER.to_string(), log_dir.path());
    telemetry.start();
    let mut config = config_with_fallback_settings(30, 8);
    config.validation.stderr_cap_bytes = 8;
    let handler = OutcomeHandler::new(config, telemetry.clone());

    let result = handler
        .handle(
            &store,
            &bead(workspace.path(), "stderr-failure"),
            &AgentOutcome {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
            false,
        )
        .await
        .expect("stderr fallback handler");
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .expect("stderr telemetry flush");
    let events = read_telemetry(log_dir.path());
    telemetry.shutdown().await;

    assert_eq!(result.outcome, needle::types::Outcome::Failure);
    assert_eq!(
        result.bead_action,
        BeadAction::Released(ReleaseReason::GateFailed)
    );
    let actions = store.actions();
    assert!(
        actions.contains(&StoreAction::Reopen),
        "reopen action: {actions:?}"
    );
    assert!(actions.contains(&StoreAction::AddLabel("verification-failed".into())));

    let failures = telemetry_rows(&events, "verification.failed");
    assert_eq!(failures.len(), 1);
    let output = failures[0]["data"]["output"]
        .as_str()
        .expect("failure telemetry output");
    assert!(output.contains("fallback verifier"), "output: {output}");
    assert!(
        output.contains("01234567\n… [truncated at 8 bytes]"),
        "stderr must be capped at the configured byte limit: {output}"
    );
    assert!(
        !output.contains("89abcdef"),
        "uncapped stderr leaked: {output}"
    );
}

async fn assert_fallback_verifier_timeout_is_released_as_execution_error(
    environment: &IsolatedEnvironment,
) {
    install_sleeping_tool_shim(environment, "pytest");
    let log = environment.home().join("timeout.log");
    let workspace = marker_fixture("python-pytest-ini", &log);
    let store = OutcomeStore::new(bead(workspace.path(), "timeout-failure"));
    let log_dir = tempfile::tempdir().expect("timeout telemetry directory");
    let telemetry = Telemetry::with_log_dir(WORKER.to_string(), log_dir.path());
    telemetry.start();
    let handler = OutcomeHandler::new(config_with_fallback_settings(1, 4096), telemetry.clone());

    let result = handler
        .handle(
            &store,
            &bead(workspace.path(), "timeout-failure"),
            &AgentOutcome {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
            false,
        )
        .await
        .expect("timeout fallback handler");
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .expect("timeout telemetry flush");
    let events = read_telemetry(log_dir.path());
    telemetry.shutdown().await;

    assert_eq!(result.outcome, needle::types::Outcome::Failure);
    assert_eq!(
        result.bead_action,
        BeadAction::Released(ReleaseReason::AgentNotFound)
    );
    let actions = store.actions();
    assert!(
        !actions.contains(&StoreAction::AddLabel("verification-failed".into())),
        "a timeout is an execution error, not a failed verification: {actions:?}"
    );
    let errors = telemetry_rows(&events, "gate.execution_error");
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0]["data"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("timed out")),
        "timeout reason should be recorded: {errors:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn clean_no_verifier_pass_is_counted_and_dirty_no_verifier_fails() {
    let _environment = IsolatedEnvironment::new(false);
    let clean = tempfile::tempdir().expect("clean no-verifier fixture");
    write_file(clean.path(), "README.md", "documentation only\n");
    commit_fixture(clean.path(), &["README.md"]);

    let (clean_result, clean_actions, clean_events) =
        run_no_verifier(clean.path(), "clean-no-verifier").await;
    assert_eq!(clean_result.outcome, needle::types::Outcome::Success);
    assert_eq!(clean_result.bead_action, BeadAction::Closed);
    assert!(clean_actions.contains(&StoreAction::Flush));
    let no_verifier_rows = telemetry_rows(&clean_events, "gate.no_verifier");
    assert_eq!(
        no_verifier_rows.len(),
        1,
        "WARN count event: {clean_events}"
    );
    assert_eq!(no_verifier_rows[0]["data"]["reason"], "not_detected");

    let dirty = tempfile::tempdir().expect("dirty no-verifier fixture");
    write_file(dirty.path(), "README.md", "documentation only\n");
    commit_fixture(dirty.path(), &["README.md"]);
    write_file(dirty.path(), "uncommitted.md", "not in the extraction\n");

    let (dirty_result, dirty_actions, dirty_events) =
        run_no_verifier(dirty.path(), "dirty-no-verifier").await;
    assert_eq!(dirty_result.outcome, needle::types::Outcome::Failure);
    assert_eq!(
        dirty_result.bead_action,
        BeadAction::Released(ReleaseReason::GateFailed)
    );
    assert!(dirty_actions.contains(&StoreAction::Reopen));
    assert!(dirty_actions.contains(&StoreAction::AddLabel("verification-failed".into())));
    assert_eq!(
        telemetry_rows(&dirty_events, "verification.failed").len(),
        1
    );
}

async fn run_no_verifier(
    workspace: &Path,
    id: &str,
) -> (needle::types::HandlerResult, Vec<StoreAction>, String) {
    let store = OutcomeStore::new(bead(workspace, id));
    let log_dir = tempfile::tempdir().expect("no-verifier telemetry directory");
    let telemetry = Telemetry::with_log_dir(WORKER.to_string(), log_dir.path());
    telemetry.start();
    let handler = OutcomeHandler::new(config_without_default_gates(), telemetry.clone());
    let result = handler
        .handle(
            &store,
            &bead(workspace, id),
            &AgentOutcome {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
            false,
        )
        .await
        .expect("no-verifier handler");
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .expect("no-verifier telemetry flush");
    let events = read_telemetry(log_dir.path());
    let actions = store.actions();
    telemetry.shutdown().await;
    (result, actions, events)
}

#[tokio::test(flavor = "current_thread")]
async fn fallback_gate_false_skips_execution_and_logs_the_dispatch_opt_out() {
    let environment = IsolatedEnvironment::new(true);
    let log = environment.home().join("opt-out.log");
    install_tool_shim(&environment, "go", &log, 7);
    let workspace = tempfile::tempdir().expect("opt-out fixture");
    write_file(
        workspace.path(),
        ".needle.yaml",
        "validation:\n  fallback_gate: false\n",
    );
    write_file(workspace.path(), "go.mod", "module example.test/opt-out\n");

    let store = OutcomeStore::new(bead(workspace.path(), "opt-out"));
    let log_dir = tempfile::tempdir().expect("opt-out telemetry directory");
    let telemetry = Telemetry::with_log_dir(WORKER.to_string(), log_dir.path());
    telemetry.start();
    let handler = OutcomeHandler::new(config_without_default_gates(), telemetry.clone());
    let (result, logs) = capture_logs(handler.handle(
        &store,
        &bead(workspace.path(), "opt-out"),
        &AgentOutcome {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        },
        false,
    ))
    .await;
    let result = result.expect("opt-out handler");
    telemetry.shutdown().await;

    assert_eq!(result.outcome, needle::types::Outcome::Success);
    assert_eq!(result.bead_action, BeadAction::Closed);
    assert!(!log.exists(), "the opted-out verifier was executed");
    assert!(
        logs.contains("validation.fallback_gate is false"),
        "opt-out must be logged at dispatch: {logs}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn gate_less_committed_compile_failure_reopens_and_releases_with_verification_failed() {
    let _environment = IsolatedEnvironment::new(false);
    let workspace = tempfile::tempdir().expect("broken Cargo fixture");
    write_file(
        workspace.path(),
        "Cargo.toml",
        "[package]\nname = \"broken-fallback\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[lib]\npath = \"lib.rs\"\n",
    );
    write_file(
        workspace.path(),
        "lib.rs",
        "pub fn broken() -> i32 { \"this does not compile\" }\n",
    );
    commit_fixture(workspace.path(), &["Cargo.toml", "lib.rs"]);

    let store = OutcomeStore::new(bead(workspace.path(), "compile-failure"));
    let log_dir = tempfile::tempdir().expect("compile-failure telemetry directory");
    let telemetry = Telemetry::with_log_dir(WORKER.to_string(), log_dir.path());
    telemetry.start();
    let handler = OutcomeHandler::new(config_without_default_gates(), telemetry.clone());
    let result = handler
        .handle(
            &store,
            &bead(workspace.path(), "compile-failure"),
            &AgentOutcome {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
            false,
        )
        .await
        .expect("compile-failure handler");
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .expect("compile-failure telemetry flush");
    let events = read_telemetry(log_dir.path());
    telemetry.shutdown().await;

    assert_eq!(result.outcome, needle::types::Outcome::Failure);
    assert_eq!(
        result.bead_action,
        BeadAction::Released(ReleaseReason::GateFailed)
    );
    let actions = store.actions();
    assert!(
        actions.contains(&StoreAction::Reopen),
        "reopen action: {actions:?}"
    );
    assert!(actions.contains(&StoreAction::AddLabel("verification-failed".into())));
    let failures = telemetry_rows(&events, "verification.failed");
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["data"]["command"], "fallback_rust");
    assert!(
        failures[0]["data"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("cargo build --all-targets")),
        "failure telemetry should carry the selected verifier output"
    );

    assert_armor_no_config_dispatch_reopens_non_compiling_commit(&_environment).await;
}

async fn assert_armor_no_config_dispatch_reopens_non_compiling_commit(
    _environment: &IsolatedEnvironment,
) {
    let workspace = tempfile::tempdir().expect("ARMOR fixture");
    write_file(
        workspace.path(),
        "go.mod",
        "module github.com/jedarden/armor\n\ngo 1.20\n",
    );
    write_file(
        workspace.path(),
        "cmd/armor/main.go",
        "package main\n\nfunc main() { var _ int = \"not an int\" }\n",
    );
    commit_fixture(workspace.path(), &["go.mod", "cmd/armor/main.go"]);
    assert!(!workspace.path().join(".needle.yaml").exists());

    let bead = bead(workspace.path(), "armor-no-config");
    let store = Arc::new(OutcomeStore::new(bead.clone()));
    let log_dir = tempfile::tempdir().expect("ARMOR telemetry directory");
    let telemetry = Telemetry::with_log_dir(WORKER.to_string(), log_dir.path());
    telemetry.start();

    let adapter = AgentAdapter {
        name: "armor-agent-fixture".to_string(),
        description: None,
        agent_cli: "fixture-agent".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: "printf 'agent completed\\n'".to_string(),
        environment: HashMap::new(),
        timeout_secs: 5,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        usage_format: None,
        output_transform: None,
        harness: None,
        harness_version: None,
    };
    let mut adapters = HashMap::new();
    adapters.insert(adapter.name.clone(), adapter);
    let dispatcher = Dispatcher::with_adapters(adapters, telemetry.clone(), 5)
        .with_bead_store(store.clone())
        .with_worker_id(WORKER.to_string());
    let prompt = BuiltPrompt {
        content: "ARMOR no-config dispatch fixture".to_string(),
        hash: "armor-fixture-prompt".to_string(),
        token_estimate: 5,
        template_name: "armor-fixture".to_string(),
        template_version: "1".to_string(),
    };
    // Pre-spawn verification is context-only: carry the claim this fixture
    // store confirms.
    let context = DispatchContext::new(
        ResolvedStoreContext::new(store.clone(), workspace.path().to_path_buf()),
        ClaimIdentity {
            actor: WORKER.to_string(),
            revision: Some(1),
            claim_epoch: Some(1),
        },
    );
    let execution = dispatcher
        .dispatch_with_context(
            &bead.id,
            &prompt,
            dispatcher
                .adapter("armor-agent-fixture")
                .expect("fixture adapter"),
            workspace.path(),
            &context,
        )
        .await
        .expect("fixture agent dispatch");
    drop(dispatcher);

    assert_eq!(execution.exit_code, 0);
    let handler = OutcomeHandler::new(config_without_default_gates(), telemetry.clone());
    let result = handler
        .handle(
            store.as_ref(),
            &bead,
            &AgentOutcome {
                exit_code: execution.exit_code,
                stdout: execution.stdout,
                stderr: execution.stderr,
            },
            false,
        )
        .await
        .expect("ARMOR fallback handler");
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .expect("ARMOR telemetry flush");
    let events = read_telemetry(log_dir.path());
    telemetry.shutdown().await;

    assert_eq!(result.outcome, needle::types::Outcome::Failure);
    assert_eq!(
        result.bead_action,
        BeadAction::Released(ReleaseReason::GateFailed)
    );
    let actions = store.actions();
    assert!(
        actions.contains(&StoreAction::Reopen),
        "reopen action: {actions:?}"
    );
    assert!(actions.contains(&StoreAction::AddLabel("verification-failed".into())));
    let failures = telemetry_rows(&events, "verification.failed");
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["data"]["command"], "fallback_go");
    assert!(
        failures[0]["data"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("go build ./...")),
        "ARMOR failure should identify the Go fallback command: {failures:?}"
    );
}

fn read_telemetry(log_dir: &Path) -> String {
    std::fs::read_dir(log_dir)
        .expect("telemetry directory")
        .flatten()
        .find_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .unwrap_or_default()
}

fn telemetry_rows(events: &str, event_type: &str) -> Vec<serde_json::Value> {
    events
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event_type"] == event_type)
        .collect()
}

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

struct CapturedLogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CapturedLogWriter(self.0.clone())
    }
}

async fn capture_logs<F: std::future::Future>(future: F) -> (F::Output, String) {
    let captured = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .without_time()
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let output = future.await;
    drop(_guard);
    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).expect("captured UTF-8");
    (output, logs)
}
