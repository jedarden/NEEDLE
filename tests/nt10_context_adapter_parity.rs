//! N-T10 conformance: CLI, daemon, and adapter policy/context parity.
//!
//! The CLI policy doctor, the supervisor's worker configuration, and the
//! adapter registry must all identify the same workspace and adapter before
//! they resolve policy.  A conflict must also fail identically at every
//! boundary rather than allowing one surface to run with a different context.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Mutex, MutexGuard, OnceLock};

use needle::config::{CliOverrides, Config, ConfigLoader};
use needle::dispatch::{builtin_adapters, load_adapters};
use needle::needle_learning::AttemptId;
use needle::policy::{
    AuthorityRegistry, PolicyError, PolicyKind, PolicyScope, PolicySource, ResolutionContext,
};
use needle::supervisor::SupervisorConfig;
use serde_json::Value;
use tempfile::TempDir;

fn needle_binary() -> OsString {
    std::env::var_os("NEXTEST_BIN_EXE_needle")
        .unwrap_or_else(|| OsString::from(env!("CARGO_BIN_EXE_needle")))
}

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct IsolatedEnvironment {
    saved: Vec<(&'static str, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl IsolatedEnvironment {
    fn enter(fixture: &Fixture) -> Self {
        let lock = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let keys = [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "TMPDIR",
            "NEEDLE_STATE_DIR",
            "NEEDLE_TEST_HARNESS",
            "NEEDLE_INNER",
        ];
        let saved = keys
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();

        std::env::set_var("HOME", &fixture.home);
        std::env::set_var("XDG_CONFIG_HOME", fixture.home.join(".config"));
        std::env::set_var("XDG_STATE_HOME", fixture.home.join(".local/state"));
        std::env::set_var("XDG_CACHE_HOME", fixture.home.join(".cache"));
        std::env::set_var("TMPDIR", fixture.root.path().join("tmp"));
        std::env::set_var("NEEDLE_STATE_DIR", fixture.root.path().join("state"));
        std::env::remove_var("NEEDLE_TEST_HARNESS");
        std::env::remove_var("NEEDLE_INNER");

        Self { saved, _lock: lock }
    }
}

impl Drop for IsolatedEnvironment {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

struct Fixture {
    root: TempDir,
    home: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("create isolated N-T10 fixture");
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let adapters = root.path().join("adapters");
        fs::create_dir_all(home.join(".config/needle")).expect("create isolated config");
        fs::create_dir_all(&workspace).expect("create isolated workspace");
        fs::create_dir_all(&adapters).expect("create isolated adapters directory");

        fs::write(
            home.join(".config/needle/config.yaml"),
            format!(
                "agent:\n  default: parity-adapter\n  adapters_dir: {}\nprompt:\n  instructions: adapter-neutral instructions\n  templates:\n    pluck: parity template\n",
                yaml_path(&adapters)
            ),
        )
        .expect("write isolated global config");
        fs::write(
            workspace.join(".needle.yaml"),
            "bead_cli:\n  backend: bead-rs\nprompt:\n  context_files:\n    - AGENTS.md\n",
        )
        .expect("write isolated workspace config");
        fs::write(workspace.join("AGENTS.md"), "repository policy\n")
            .expect("write repository policy");
        fs::write(workspace.join("CLAUDE.md"), "adapter projection\n")
            .expect("write adapter projection");
        fs::write(
            adapters.join("parity-adapter.yaml"),
            "name: parity-adapter\ndescription: deterministic parity adapter\nagent_cli: /bin/true\ninvoke_template: /bin/true\nprovider: parity-provider\nmodel: parity-model\n",
        )
        .expect("write isolated adapter");

        Self {
            root,
            home,
            workspace,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(needle_binary());
        command
            .current_dir(&self.workspace)
            .env("HOME", &self.home)
            .env("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT", self.root.path())
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_STATE_HOME", self.home.join(".local/state"))
            .env("XDG_CACHE_HOME", self.home.join(".cache"))
            .env("TMPDIR", self.root.path().join("tmp"))
            .env("NEEDLE_STATE_DIR", self.root.path().join("state"))
            .env_remove("NEEDLE_TEST_HARNESS")
            .env_remove("NEEDLE_INNER");
        command
    }

    fn run_policy_doctor(&self) -> Output {
        self.command()
            .args([
                "doctor",
                "policy",
                "--workspace",
                self.workspace.to_str().expect("workspace path is UTF-8"),
                "--adapter",
                "parity-adapter",
                "--json",
            ])
            .output()
            .expect("run isolated policy doctor")
    }
}

fn yaml_path(path: &Path) -> String {
    format!("'{}'", path.display())
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "policy doctor stdout should be JSON ({error}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn insert(registry: &mut AuthorityRegistry, source: PolicySource) {
    registry
        .insert(source)
        .expect("N-T10 fixture source IDs are unique");
}

fn source(id: &Path, kind: PolicyKind, scope: PolicyScope) -> PolicySource {
    PolicySource::new(
        id.display().to_string(),
        kind,
        scope,
        fs::read_to_string(id).expect("N-T10 fixture policy source is readable"),
    )
    .expect("N-T10 fixture source is valid")
}

fn fixture_registry(fixture: &Fixture, config: &Config) -> AuthorityRegistry {
    let mut registry = AuthorityRegistry::new();
    insert(
        &mut registry,
        source(
            &fixture.home.join(".config/needle/config.yaml"),
            PolicyKind::ExecutableGate,
            PolicyScope::Global,
        ),
    );
    insert(
        &mut registry,
        source(
            &fixture.workspace.join(".needle.yaml"),
            PolicyKind::ExecutableGate,
            PolicyScope::repository(&fixture.workspace),
        ),
    );
    insert(
        &mut registry,
        source(
            &fixture.workspace.join("AGENTS.md"),
            PolicyKind::RepositoryInstructions,
            PolicyScope::repository(&fixture.workspace),
        ),
    );
    insert(
        &mut registry,
        source(
            &fixture.workspace.join("CLAUDE.md"),
            PolicyKind::AdapterInstructions,
            PolicyScope::directory(&fixture.workspace),
        ),
    );

    if let Some(instructions) = &config.prompt.instructions {
        insert(
            &mut registry,
            PolicySource::new(
                "config:prompt.instructions",
                PolicyKind::AdapterInstructions,
                PolicyScope::adapter("parity-adapter"),
                instructions.clone(),
            )
            .expect("prompt instructions source is valid"),
        );
    }
    if !config.prompt.templates.is_empty() {
        insert(
            &mut registry,
            PolicySource::new(
                "config:prompt.templates",
                PolicyKind::AdapterInstructions,
                PolicyScope::adapter("parity-adapter"),
                serde_json::to_string(&config.prompt.templates)
                    .expect("prompt templates are serializable"),
            )
            .expect("prompt templates source is valid"),
        );
    }
    if fixture.workspace.join("AGENTS.local.md").is_file() {
        insert(
            &mut registry,
            source(
                &fixture.workspace.join("AGENTS.local.md"),
                PolicyKind::RepositoryInstructions,
                PolicyScope::repository(&fixture.workspace),
            ),
        );
    }
    registry
}

fn resolved_hash(registry: &AuthorityRegistry, workspace: &Path, adapter: &str) -> String {
    registry
        .resolve(&ResolutionContext::new(workspace).for_adapter(adapter))
        .expect("fixture policy is unambiguous")
        .hash()
        .expect("fixture policy hash is valid")
}

#[test]
fn cli_daemon_and_adapter_share_policy_context_and_manifest_identity() {
    let fixture = Fixture::new();
    let _environment = IsolatedEnvironment::enter(&fixture);

    let cli = fixture.run_policy_doctor();
    assert_eq!(
        cli.status.code(),
        Some(0),
        "CLI policy doctor must admit the fixture:\n{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let report = json(&cli);
    assert_eq!(report["status"], "pass");
    assert_eq!(report["adapter"], "parity-adapter");
    assert_eq!(report["manifest"]["version"], "effective-policy-v1");

    let (config, _) = ConfigLoader::load_resolved(
        &fixture.workspace,
        CliOverrides {
            workspace: Some(fixture.workspace.clone()),
            ..Default::default()
        },
    )
    .expect("daemon configuration resolves from the isolated fixture");
    let daemon = SupervisorConfig::from_config(&config);
    let daemon_adapter = daemon
        .agent
        .clone()
        .expect("daemon carries adapter identity");
    assert_eq!(daemon.workspace, fixture.workspace);
    assert_eq!(daemon_adapter, "parity-adapter");

    let adapters = load_adapters(&config.agent.adapters_dir, &builtin_adapters())
        .expect("adapter registry resolves from the isolated fixture");
    let adapter = adapters
        .get(&daemon_adapter)
        .expect("daemon-selected adapter is in the adapter registry");
    assert_eq!(adapter.name, daemon_adapter);
    assert_eq!(adapter.provider.as_deref(), Some("parity-provider"));
    assert_eq!(adapter.model.as_deref(), Some("parity-model"));

    let registry = fixture_registry(&fixture, &config);
    let daemon_context = ResolutionContext::new(&daemon.workspace).for_adapter(&daemon_adapter);
    let adapter_context = ResolutionContext::new(&fixture.workspace).for_adapter(&adapter.name);
    assert_eq!(daemon_context, adapter_context);

    let expected_hash = resolved_hash(&registry, &fixture.workspace, "parity-adapter");
    assert_eq!(report["manifest"]["hash"], expected_hash);

    let daemon_manifest = registry
        .resolve(&daemon_context)
        .expect("daemon context resolves")
        .materialize_context_manifest(
            AttemptId::new("attempt-nt10-parity-daemon").expect("valid attempt ID"),
        )
        .expect("daemon context materializes");
    let adapter_manifest = registry
        .resolve(&adapter_context)
        .expect("adapter context resolves")
        .materialize_context_manifest(
            AttemptId::new("attempt-nt10-parity-adapter").expect("valid attempt ID"),
        )
        .expect("adapter context materializes");
    assert_eq!(daemon_manifest.policy, adapter_manifest.policy);
    assert_eq!(
        daemon_manifest.policy.digest.to_string(),
        expected_hash,
        "daemon and adapter must record the same effective policy digest"
    );

    let expected_sources = registry
        .resolve(&daemon_context)
        .expect("fixture policy is unambiguous")
        .source_ids()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let reported_sources = report["manifest"]["source_ids"]
        .as_array()
        .expect("CLI reports manifest source IDs")
        .iter()
        .map(|source| source.as_str().expect("manifest source ID is text"))
        .collect::<Vec<_>>();
    assert_eq!(reported_sources, expected_sources);
}

#[test]
fn cli_daemon_and_adapter_reject_the_same_ambiguous_context() {
    let fixture = Fixture::new();
    fs::write(
        fixture.workspace.join("AGENTS.local.md"),
        "conflicting repository policy\n",
    )
    .expect("write deterministic conflicting policy");
    let _environment = IsolatedEnvironment::enter(&fixture);

    let first = fixture.run_policy_doctor();
    let second = fixture.run_policy_doctor();
    assert_eq!(first.status.code(), Some(1));
    assert_eq!(second.status.code(), Some(1));
    assert_eq!(
        first.stdout, second.stdout,
        "CLI conflict output is deterministic"
    );
    let report = json(&first);
    assert_eq!(report["status"], "fail");
    assert_eq!(report["manifest"], Value::Null);
    assert_eq!(report["conflicts"][0]["authority"], "repository");

    let (config, _) = ConfigLoader::load_resolved(
        &fixture.workspace,
        CliOverrides {
            workspace: Some(fixture.workspace.clone()),
            ..Default::default()
        },
    )
    .expect("daemon configuration resolves from the isolated fixture");
    let daemon = SupervisorConfig::from_config(&config);
    let adapters = load_adapters(&config.agent.adapters_dir, &builtin_adapters())
        .expect("adapter registry resolves from the isolated fixture");
    let adapter = adapters
        .get(daemon.agent.as_deref().expect("daemon adapter"))
        .expect("daemon adapter is loaded");
    let registry = fixture_registry(&fixture, &config);

    let daemon_error = registry
        .resolve(
            &ResolutionContext::new(&daemon.workspace)
                .for_adapter(daemon.agent.as_deref().expect("daemon adapter")),
        )
        .expect_err("daemon context must fail closed on policy ambiguity");
    let adapter_error = registry
        .resolve(&ResolutionContext::new(&fixture.workspace).for_adapter(&adapter.name))
        .expect_err("adapter context must fail closed on policy ambiguity");
    assert_eq!(daemon_error, adapter_error);
    match &daemon_error {
        PolicyError::Conflict { sources, .. } => assert_eq!(
            sources
                .iter()
                .map(|source| source.as_str())
                .collect::<Vec<_>>(),
            vec![
                fixture
                    .workspace
                    .join("AGENTS.local.md")
                    .display()
                    .to_string(),
                fixture.workspace.join("AGENTS.md").display().to_string(),
            ]
        ),
        other => panic!("expected a policy conflict, got {other:?}"),
    }

    let source_ids = report["conflicts"][0]["source_ids"]
        .as_array()
        .expect("CLI reports conflict source IDs")
        .iter()
        .map(|source| source.as_str().expect("source ID is text").to_owned())
        .collect::<Vec<_>>();
    let mut sorted = source_ids.clone();
    sorted.sort();
    assert_eq!(source_ids, sorted, "CLI conflict IDs are deterministic");
    assert_eq!(source_ids.len(), 2);
}
