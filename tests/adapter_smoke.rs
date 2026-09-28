//! Isolated smoke fixtures for every adapter the README advertises.
//!
//! The README's "Supported agents" matrix names Claude Code, OpenCode, Codex,
//! and Aider. Dispatch-level invocation is covered by the fake-CLI contracts
//! in `integration_spawn/builtin_adapter_invocation.rs`; what was missing is
//! a smoke check of each adapter's *configuration* against the CLI it claims
//! to drive — the gap that let the OpenCode built-in ship an invoke template
//! built from flags (`--prompt-file`, `--non-interactive`) that no real
//! opencode ever accepted, while every fake-CLI test passed because the fake
//! accepted anything (found live against opencode 1.18.29 on 2026-09-26).
//!
//! Two fixture families pin that contract without a network, an LLM call, or
//! a real agent CLI on PATH:
//!
//! 1. **Recorded CLI flag lists** (`tests/fixtures/agent-clis/*.flags.txt`)
//!    — one file per CLI, recorded from the real `--help` (claude 2.1.283,
//!    opencode 1.18.29, codex 0.157.0, live on codinghome 2026-09-26) or
//!    extracted from aider's `args.py` on `main` (no released CLI was
//!    available). Every `--flag` a built-in adapter's invoke template passes
//!    must appear in its CLI's list.
//! 2. **`needle test-agent` smoke** — the same validation command the README
//!    tells users to run, executed against stub CLIs in a temp bin dir, for
//!    every documented adapter. Stubs answer `--version` and `--help` probes;
//!    nothing dispatches a prompt and nothing leaves the tempdir.
//!
//! Both mutate `PATH`, so every test serializes on the module's PATH lock.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use needle::config::{AgentConfig, Config};
use needle::dispatch::{
    builtin_adapters, load_adapters, render_template, test_agent, AgentAdapter, AgentTestStatus,
    Dispatcher, ExecutionResult, TimeoutReason,
};
use needle::prompt::BuiltPrompt;
use needle::telemetry::Telemetry;
use needle::types::BeadId;
use tempfile::tempdir;

const CLAUDE_FLAGS: &str = include_str!("fixtures/agent-clis/claude.flags.txt");
const OPENCODE_FLAGS: &str = include_str!("fixtures/agent-clis/opencode.flags.txt");
const CODEX_FLAGS: &str = include_str!("fixtures/agent-clis/codex.flags.txt");
const AIDER_FLAGS: &str = include_str!("fixtures/agent-clis/aider.flags.txt");
const README: &str = include_str!("../README.md");

/// Serializes PATH mutation among the tests in this binary.
static PATH_LOCK: Mutex<()> = Mutex::new(());

/// Restore `PATH` on drop, however the test ends.
struct PathGuard {
    previous: Option<std::ffi::OsString>,
}

impl PathGuard {
    fn prepend(dir: &Path) -> Self {
        let previous = std::env::var_os("PATH");
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(
            previous.as_deref().unwrap_or_default(),
        ));
        std::env::set_var("PATH", std::env::join_paths(&paths).expect("join PATH"));
        PathGuard { previous }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
    }
}

/// Every adapter name the README matrix advertises, paired with the recorded
/// flag list of the CLI it drives. `generic` is the copy-me template, not a
/// runnable adapter, so it is absent here and asserted separately.
const DOCUMENTED: [(&str, &str); 6] = [
    ("claude", CLAUDE_FLAGS),
    ("claude-sonnet", CLAUDE_FLAGS),
    ("claude-opus", CLAUDE_FLAGS),
    ("opencode", OPENCODE_FLAGS),
    ("codex", CODEX_FLAGS),
    ("aider", AIDER_FLAGS),
];

fn builtin(name: &str) -> AgentAdapter {
    builtin_adapters()
        .into_iter()
        .find(|adapter| adapter.name == name)
        .unwrap_or_else(|| panic!("builtin adapter {name} missing from the registry"))
}

/// Flags offered by a recorded fixture: one per line, `#` lines are
/// provenance comments.
fn fixture_flags(fixture: &str) -> Vec<&str> {
    fixture
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter(|line| line.starts_with('-'))
        .collect()
}

/// Every `--flag`-shaped token in an invoke template, placeholder-free.
///
/// Tokens are split on whitespace and stripped of shell quoting; `--` alone
/// (the args-input stdin marker) is not a flag.
fn template_flags(adapter: &AgentAdapter) -> Vec<String> {
    adapter
        .invoke_template
        .split_whitespace()
        .map(|token| token.trim_matches(|c| c == '"' || c == '\''))
        .filter(|token| token.starts_with('-') && *token != "--" && !token.starts_with('{'))
        .map(str::to_string)
        .collect()
}

#[test]
fn documented_adapter_flags_exist_in_recorded_cli_help() {
    for (name, fixture) in DOCUMENTED {
        let adapter = builtin(name);
        let offered = fixture_flags(fixture);
        assert!(
            !offered.is_empty(),
            "flag fixture for {name} must not be empty"
        );
        for flag in template_flags(&adapter) {
            assert!(
                offered.contains(&flag.as_str()),
                "{name} invokes `{flag}`, which its recorded CLI flag list does not offer — \
                 the template has drifted from the real CLI"
            );
        }
    }
}

#[test]
fn readme_support_matrix_matches_registered_builtins() {
    let registry: Vec<String> = builtin_adapters().into_iter().map(|a| a.name).collect();
    for name in DOCUMENTED.iter().map(|(name, _)| *name) {
        assert!(
            registry.iter().any(|registered| registered == name),
            "README advertises `{name}` but the built-in registry does not contain it"
        );
    }
    for row in [
        "| Claude Code |",
        "| OpenCode |",
        "| Codex CLI |",
        "| Aider |",
    ] {
        assert!(README.contains(row), "README support matrix omitted {row}");
    }
    for invocation in [
        "opencode run --format json --auto",
        "codex exec --model {model} --sandbox workspace-write --json",
        "aider --model {model} --yes-always --message",
    ] {
        assert!(
            README.contains(invocation),
            "README support matrix omitted invocation `{invocation}`"
        );
    }
    assert!(
        README.contains("not a supported executable"),
        "README must narrow the generic placeholder claim"
    );
}

/// A temp bin dir with stub CLIs and transforms that satisfy test-agent's
/// probes: `--version` prints a stub version, everything else is a no-op that
/// exits 0.
struct StubBin {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl StubBin {
    fn new(names: &[&str]) -> Self {
        let dir = tempdir().expect("create stub bin dir");
        for name in names {
            let path = dir.path().join(name);
            fs::write(
                &path,
                "#!/usr/bin/env bash\nif [ \"$1\" = '--version' ]; then echo '1.0.0-stub'; fi\nexit 0\n",
            )
            .expect("write stub script");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .expect("make stub executable");
        }
        StubBin {
            path: dir.path().to_path_buf(),
            _dir: dir,
        }
    }
}

/// A config whose adapter directory is an empty tempdir, so only the
/// built-ins resolve and nothing reads the real home.
fn isolated_config(adapters_dir: &Path) -> Config {
    Config {
        agent: AgentConfig {
            default: "claude".to_string(),
            args: vec![],
            timeout: 60,
            adapters_dir: adapters_dir.to_path_buf(),
            routing: None,
            evidence_routing: Default::default(),
        },
        ..Default::default()
    }
}

#[test]
fn test_agent_reports_ready_for_every_documented_adapter() {
    let _path_lock = PATH_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let stubs = StubBin::new(&[
        "claude",
        "opencode",
        "codex",
        "aider",
        "needle-transform-claude",
        "needle-transform-codex",
    ]);
    let _guard = PathGuard::prepend(&stubs.path);
    let empty_adapters = tempdir().expect("create empty adapter dir");
    let config = isolated_config(empty_adapters.path());

    // (adapter, expected input-method label, output_transform wired).
    let cases: [(&str, &str, bool); 6] = [
        ("claude", "stdin", true),
        ("claude-sonnet", "stdin", true),
        ("claude-opus", "stdin", true),
        ("opencode", "stdin", false),
        ("codex", "args (--)", true),
        ("aider", "args (--message)", false),
    ];

    for (name, input_method, transform_wired) in cases {
        let result = test_agent(name, &config).expect("test_agent should validate");
        assert_eq!(
            result.status,
            AgentTestStatus::Ready,
            "{name} must smoke-test READY, errors: {:?}",
            result.errors
        );
        assert!(result.cli_path.is_some(), "{name} CLI must resolve on PATH");
        assert!(
            result.version.as_deref().is_some_and(|v| !v.is_empty()),
            "{name} version probe must return text"
        );
        assert_eq!(result.input_method, input_method, "{name} input method");
        assert!(result.probe_result.is_some(), "{name} probe must run");
        assert_eq!(
            result.output_transform_ok.is_some(),
            transform_wired,
            "{name} output_transform wiring"
        );
        assert!(
            result.errors.is_empty(),
            "{name} reported errors: {:?}",
            result.errors
        );
    }
}

#[test]
fn test_agent_flags_missing_cli_and_unknown_adapter() {
    let _path_lock = PATH_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Only claude is stubbed: the generic template's `my-agent` and the
    // unknown name must both fail closed.
    let stubs = StubBin::new(&["claude", "needle-transform-claude"]);
    let _guard = PathGuard::prepend(&stubs.path);
    let empty_adapters = tempdir().expect("create empty adapter dir");
    let config = isolated_config(empty_adapters.path());

    let generic = test_agent("generic", &config).expect("test_agent should validate");
    assert_eq!(
        generic.status,
        AgentTestStatus::Error,
        "generic is a copy-me template: its placeholder CLI must not smoke-test READY"
    );
    assert!(
        generic
            .errors
            .iter()
            .any(|error| error.contains("not found on PATH")),
        "generic must report the missing CLI, errors: {:?}",
        generic.errors
    );

    assert!(
        test_agent("no-such-adapter", &config).is_err(),
        "an unknown adapter name must fail closed, not report a status"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Managed GLM-5.3-Flash adapter profiles (needle-0810eb14)
//
// The four fleet profiles in fleet/ex44/adapters/ replace the ad-hoc
// home-directory adapter with a reviewable, reproducible source of truth.
// Three fixture families pin the contract:
//
// 1. **Load-time validation** — `load_adapters` runs the declared `profile:`
//    block against the template it describes, so a contradictory
//    model/context/effort setting fails to load instead of silently
//    diverging.
// 2. **Rendered invocation** — every profile's template renders to valid
//    shell, passes only flags the recorded claude 2.1.283 help offers, keeps
//    the streamed-output contract, and does not mask the agent's exit
//    status.
// 3. **Fake-CLI dispatch** — the shipped templates (pointed at a fake CLI by
//    absolute path, flags untouched) run through the production spawn path:
//    the real exit status propagates, partial streaming keeps the idle
//    deadline alive, signals and both timeout flavors report structurally,
//    and the 1M canary's bracketed identifier survives quoting.
// ──────────────────────────────────────────────────────────────────────────────

/// The repo-managed adapter source directory.
fn fleet_adapters_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fleet/ex44/adapters")
}

/// Every managed GLM-5.3-Flash profile: (name, effort, context_mode, max_turns).
const MANAGED_PROFILES: [(&str, &str, &str, u64); 4] = [
    ("claude-code-glm-5.3-flash", "max", "standard", 100),
    ("claude-code-glm-5.3-flash-1m", "max", "1m", 100),
    ("claude-code-glm-5.3-flash-low", "low", "standard", 100),
    ("claude-code-glm-5.3-flash-high", "high", "standard", 100),
];

fn managed_adapter(name: &str) -> AgentAdapter {
    let adapters =
        load_adapters(&fleet_adapters_dir(), &[]).expect("fleet adapters directory must load");
    adapters
        .get(name)
        .unwrap_or_else(|| panic!("managed adapter {name} missing from the fleet directory"))
        .clone()
}

#[test]
fn managed_fleet_profiles_load_and_declared_facts_agree() {
    let dir = fleet_adapters_dir();
    let adapters = load_adapters(&dir, &builtin_adapters())
        .expect("fleet adapters directory must load alongside the built-ins");
    for (name, effort, context_mode, max_turns) in MANAGED_PROFILES {
        let adapter = adapters
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing from the loaded fleet adapters"));
        let profile = needle::adapter_profile::find_profile(&dir, name)
            .expect("profile scan must not fail")
            .unwrap_or_else(|| panic!("{name} ships no profile block"));
        assert_eq!(profile.version, "1", "{name} profile version");
        assert_eq!(profile.effort, effort, "{name} declared effort");
        assert_eq!(profile.context_mode, context_mode, "{name} context mode");
        assert_eq!(profile.max_turns, max_turns, "{name} turn ceiling");
        assert_eq!(
            profile.min_cli_version.as_deref(),
            Some("2.1.283"),
            "{name} minimum CLI version"
        );
        let expected_model = if context_mode == "1m" {
            "glm-5.3-flash[1m]"
        } else {
            "glm-5.3-flash"
        };
        assert_eq!(
            adapter.model.as_deref(),
            Some(expected_model),
            "{name} requested model"
        );
        assert_eq!(
            adapter.provider.as_deref(),
            Some("zai-proxy"),
            "{name} provider"
        );
        // The timeout policy the profile ships: idle deadline for liveness,
        // hard ceiling for the attempt budget.
        assert_eq!(adapter.idle_timeout_secs, 900, "{name} idle timeout");
        assert_eq!(adapter.hard_timeout_secs, 3600, "{name} hard timeout");
    }
    // The pre-existing managed codex profiles still load next to them.
    assert!(
        adapters.contains_key("codex-gpt-5.6-luna-xhigh"),
        "existing managed adapters must keep loading"
    );
}

#[test]
fn managed_fleet_rendered_invocations_are_valid_shell_with_recorded_flags() {
    let bead = BeadId::from("needle-0810eb14");
    let offered = fixture_flags(CLAUDE_FLAGS);
    for (name, effort, context_mode, _turns) in MANAGED_PROFILES {
        let adapter = managed_adapter(name);
        let rendered = render_template(
            &adapter.invoke_template,
            Path::new("/tmp/glm-workspace"),
            Path::new("/tmp/glm-prompt"),
            &bead,
            adapter.model.as_deref().unwrap_or_default(),
        );

        // (a) The rendered invocation is valid shell.
        let check = std::process::Command::new("bash")
            .arg("-n")
            .arg("-c")
            .arg(&rendered)
            .output()
            .expect("run bash -n on the rendered template");
        assert!(
            check.status.success(),
            "{name} renders to invalid shell: {}",
            String::from_utf8_lossy(&check.stderr)
        );

        // (b) Every flag exists in the recorded claude 2.1.283 help.
        for flag in template_flags(&adapter) {
            assert!(
                offered.contains(&flag.as_str()),
                "{name} invokes `{flag}`, which the recorded claude flag list \
                 does not offer — the profile has drifted from the real CLI"
            );
        }

        // (c) Effort is explicit and matches the profile's declaration.
        assert_eq!(
            rendered.matches("--effort").count(),
            1,
            "{name} must pass --effort exactly once"
        );
        assert!(
            rendered.contains(&format!("--effort {effort}")),
            "{name} must pass the declared effort `{effort}`"
        );

        // (d) The context-mode contract: 1M stays on its own canary.
        if context_mode == "1m" {
            assert!(
                rendered.contains("--autocompact 1m"),
                "{name} must pair the 1M identifier with --autocompact 1m"
            );
            assert!(
                rendered.contains("'glm-5.3-flash[1m]'"),
                "{name} must quote the bracketed 1M identifier (unquoted, bash \
                 treats it as a character-class glob)"
            );
        } else {
            assert!(
                !rendered.contains("[1m]") && !rendered.contains("--autocompact"),
                "{name} declares standard context but leaks 1M settings"
            );
        }

        // (e) Exit status and streamed-partial-output contracts.
        assert!(
            !rendered.contains("| cat") && !rendered.contains("|cat"),
            "{name} masks the agent exit status with a pipe to cat"
        );
        assert!(
            rendered.contains("--output-format stream-json")
                && rendered.contains("--include-partial-messages"),
            "{name} must keep the streamed partial-output contract"
        );

        // (f) Endpoint and auth stay externalized with safe defaults.
        assert!(
            rendered.contains("ANTHROPIC_BASE_URL=${NEEDLE_ZAI_BASE_URL:-"),
            "{name} must externalize the endpoint"
        );
        assert!(
            rendered.contains("ANTHROPIC_AUTH_TOKEN=${NEEDLE_ZAI_AUTH_TOKEN:-proxy-handles-auth"),
            "{name} must keep the auth token externalized behind the proxy \
             placeholder"
        );
    }
}

#[test]
fn load_adapters_rejects_contradictory_managed_profiles() {
    let baseline = fs::read_to_string(fleet_adapters_dir().join("claude-code-glm-5.3-flash.yaml"))
        .expect("baseline fleet profile must be readable");
    let one_m = fs::read_to_string(fleet_adapters_dir().join("claude-code-glm-5.3-flash-1m.yaml"))
        .expect("1M fleet profile must be readable");

    // (label, mutated text) — each mutation breaks exactly one declared fact.
    let mutations: [(&str, String); 6] = [
        (
            "declared effort disagrees with the template",
            baseline.replace("--effort max", "--effort high"),
        ),
        (
            "1M context without --autocompact 1m",
            one_m.replace(" --autocompact 1m", ""),
        ),
        (
            "exit status masked by a trailing cat",
            baseline.replace("< {prompt_file}\"", "< {prompt_file} | cat\""),
        ),
        (
            "contradictory requested model",
            baseline.replace(
                "ANTHROPIC_MODEL='glm-5.3-flash'",
                "ANTHROPIC_MODEL='glm-5.3'",
            ),
        ),
        (
            "literal auth token",
            baseline.replace(
                "${NEEDLE_ZAI_AUTH_TOKEN:-proxy-handles-auth}",
                "sk-live-credential",
            ),
        ),
        (
            "thinking disabled request",
            baseline.replace("--effort max", "--thinking disabled --effort max"),
        ),
    ];
    for (label, text) in mutations {
        let dir = tempdir().expect("create mutation dir");
        fs::write(dir.path().join("mutated.yaml"), &text).expect("write mutated adapter");
        let error = load_adapters(dir.path(), &[])
            .expect_err("a contradictory managed profile must fail to load");
        assert!(
            label == "declared effort disagrees with the template"
                || error.to_string().contains("profile"),
            "the load error for `{label}` should name the profile block: {error:#}"
        );
    }

    // Controls: the unmutated baseline loads, and a legacy unmanaged adapter
    // with `| cat` still loads — the validation scope is managed profiles
    // only, so existing fleet files are not broken by this contract.
    let dir = tempdir().expect("create control dir");
    fs::write(
        dir.path().join("baseline.yaml"),
        baseline.replace("name: claude-code-glm-5.3-flash", "name: control-managed"),
    )
    .expect("write control managed adapter");
    fs::write(
        dir.path().join("legacy.yaml"),
        concat!(
            "name: control-legacy\n",
            "agent_cli: claude\n",
            "input_method:\n  method: stdin\n",
            "invoke_template: \"claude --print < {prompt_file} | cat\"\n",
            "timeout_secs: 60\n",
        ),
    )
    .expect("write control legacy adapter");
    let adapters =
        load_adapters(dir.path(), &[]).expect("valid managed and legacy adapters must both load");
    assert!(adapters.contains_key("control-managed"));
    assert!(adapters.contains_key("control-legacy"));
}

#[test]
fn managed_adapter_install_is_idempotent_and_keeps_a_rollback_copy() {
    let destination = tempdir().expect("create install destination");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install-needle-adapters.sh");

    let run = |names: Option<&str>| {
        let mut command = std::process::Command::new("bash");
        command
            .arg(&script)
            .arg("--adapters-dir")
            .arg(destination.path());
        if let Some(names) = names {
            command.arg("--names").arg(names);
        }
        command.output().expect("run adapter installer")
    };

    let first = run(None);
    assert!(first.status.success(), "first install failed: {:?}", first);
    let second = run(None);
    assert!(
        second.status.success(),
        "second install failed: {:?}",
        second
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        second_stdout.contains("done: 0 installed, 4 already current"),
        "second install must be a no-op: {second_stdout}"
    );

    let baseline = destination.path().join("claude-code-glm-5.3-flash.yaml");
    fs::write(&baseline, b"previous explicit flash profile\n").expect("seed rollback target");
    let replacement = run(Some("claude-code-glm-5.3-flash"));
    assert!(
        replacement.status.success(),
        "replacement install failed: {:?}",
        replacement
    );
    let backups: Vec<_> = fs::read_dir(destination.path())
        .expect("read install destination")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("claude-code-glm-5.3-flash.yaml.bak-")
        })
        .collect();
    assert_eq!(backups.len(), 1, "one prior target must be retained");
    assert_eq!(
        fs::read(backups[0].path()).expect("read rollback copy"),
        b"previous explicit flash profile\n"
    );
    assert_eq!(
        fs::read(&baseline).expect("read installed baseline"),
        fs::read(fleet_adapters_dir().join("claude-code-glm-5.3-flash.yaml"))
            .expect("read source baseline")
    );
}

#[test]
fn test_agent_reports_profile_summary_and_enforces_min_cli_version() {
    let _path_lock = PATH_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let stubs = tempdir().expect("create stub dir");
    let write_cli = |version: &str| {
        fs::write(
            stubs.path().join("claude"),
            format!("#!/usr/bin/env bash\nif [ \"$1\" = '--version' ]; then echo '{version}'; fi\nexit 0\n"),
        )
        .expect("write claude stub");
        fs::set_permissions(
            stubs.path().join("claude"),
            fs::Permissions::from_mode(0o755),
        )
        .expect("make stub executable");
        fs::write(
            stubs.path().join("needle-transform-claude"),
            "#!/usr/bin/env bash\nexit 0\n",
        )
        .expect("write transform stub");
        fs::set_permissions(
            stubs.path().join("needle-transform-claude"),
            fs::Permissions::from_mode(0o755),
        )
        .expect("make transform stub executable");
    };

    // The fleet template invokes claude by host-specific absolute path; point
    // it at the stub name so the probe machinery resolves hermetically.
    let baseline = fs::read_to_string(fleet_adapters_dir().join("claude-code-glm-5.3-flash.yaml"))
        .expect("baseline fleet profile must be readable")
        .replace("/home/coding/.local/bin/claude", "claude");
    let adapters_dir = tempdir().expect("create adapters dir");
    fs::write(
        adapters_dir.path().join("claude-code-glm-5.3-flash.yaml"),
        &baseline,
    )
    .expect("write fleet profile into the adapters dir");
    let config = isolated_config(adapters_dir.path());

    write_cli("2.1.283 (Claude Code)");
    let _guard = PathGuard::prepend(stubs.path());
    let result = test_agent("claude-code-glm-5.3-flash", &config)
        .expect("test_agent should validate the managed profile");
    assert_eq!(
        result.status,
        AgentTestStatus::Ready,
        "errors: {:?}",
        result.errors
    );
    let summary = result
        .profile_summary
        .as_deref()
        .expect("a managed profile must surface its summary");
    assert!(
        summary.contains("effort=max")
            && summary.contains("context=standard")
            && summary.contains("max-turns=100")
            && summary.contains("min-cli=2.1.283"),
        "profile summary: {summary}"
    );
    assert_eq!(result.min_cli_version_ok, Some(true));

    // Below the declared floor the adapter is unusable as shipped: an ERROR,
    // the same severity as a missing executable.
    write_cli("2.0.0 (Claude Code)");
    let stale = test_agent("claude-code-glm-5.3-flash", &config)
        .expect("test_agent should validate against the old CLI");
    assert_eq!(
        stale.status,
        AgentTestStatus::Error,
        "a CLI below min_cli_version must not smoke-test READY"
    );
    assert_eq!(stale.min_cli_version_ok, Some(false));
    assert!(
        stale
            .errors
            .iter()
            .any(|error| error.contains("min_cli_version")),
        "the failure must name the version floor, errors: {:?}",
        stale.errors
    );
}

/// A fake Claude CLI exercising the behaviors the managed profiles depend on.
///
/// Logs one `arg=` line per argv entry to `$NEEDLE_FAKE_GLM_LOG`; the mode
/// env var selects the stream shape, and the exit env var overrides the exit
/// status.
struct FakeGlmClaude {
    _bin: tempfile::TempDir,
    cli_path: PathBuf,
    log: PathBuf,
}

impl FakeGlmClaude {
    fn new() -> Self {
        let bin = tempdir().expect("create fake bin dir");
        let cli_path = bin.path().join("fake-glm-claude");
        let log = bin.path().join("glm-invocations.log");
        let script = concat!(
            "#!/usr/bin/env bash\n",
            "for a in \"$@\"; do printf 'arg=%s\\n' \"$a\" >> \"$NEEDLE_FAKE_GLM_LOG\"; done\n",
            "cat > /dev/null\n",
            "case \"${NEEDLE_FAKE_GLM_MODE:-success}\" in\n",
            "  max-turns)\n",
            "    printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"model\":\"glm-5.3-flash\"}}'\n",
            "    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"error_max_turns\",\"is_error\":true,\"result\":\"max turns reached\"}'\n",
            "    ;;\n",
            "  stream-partial)\n",
            "    for i in 1 2 3 4; do\n",
            "      printf '%s\\n' \"{\\\"type\\\":\\\"stream_event\\\",\\\"partial\\\":\\\"chunk-$i\\\"}\"\n",
            "      sleep 0.6\n",
            "    done\n",
            "    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n",
            "    ;;\n",
            "  signal)\n",
            "    kill -TERM $$\n",
            "    sleep 5\n",
            "    ;;\n",
            "  timeout)\n",
            "    sleep 10\n",
            "    ;;\n",
            "  *)\n",
            "    printf '%s\\n' '{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"glm-5.3-flash\"}'\n",
            "    printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n",
            "    ;;\n",
            "esac\n",
            "exit \"${NEEDLE_FAKE_GLM_EXIT:-0}\"\n",
        );
        fs::write(&cli_path, script).expect("write fake claude script");
        fs::set_permissions(&cli_path, fs::Permissions::from_mode(0o755))
            .expect("make fake claude executable");
        FakeGlmClaude {
            _bin: bin,
            cli_path,
            log,
        }
    }
}

/// Dispatch a shipped fleet profile through the production spawn path.
///
/// The template is redirected from the host-specific claude path to the fake
/// CLI's absolute path — every flag, quoting choice, and env assignment stays
/// exactly as shipped — then `tweak` applies the scenario under test.
async fn dispatch_managed(
    name: &str,
    fake: &FakeGlmClaude,
    workspace: &Path,
    tweak: impl FnOnce(&mut AgentAdapter),
) -> ExecutionResult {
    let mut adapter = managed_adapter(name);
    adapter.invoke_template = adapter.invoke_template.replace(
        "/home/coding/.local/bin/claude",
        &fake.cli_path.display().to_string(),
    );
    adapter.environment.insert(
        "NEEDLE_FAKE_GLM_LOG".to_string(),
        fake.log.display().to_string(),
    );
    // Deadlines wide enough for every scenario except the timeout tests,
    // which tighten them through `tweak`.
    adapter.idle_timeout_secs = 60;
    adapter.hard_timeout_secs = 120;
    tweak(&mut adapter);

    let mut adapters = std::collections::HashMap::new();
    adapters.insert(name.to_string(), adapter);
    let dispatcher = Dispatcher::with_adapters(
        adapters,
        Telemetry::new("glm-profile-test".to_string()),
        3600,
    )
    .with_worker_id("glm-profile-test".to_string());
    let adapter_ref = dispatcher.adapter(name).expect("adapter wired");
    let prompt = BuiltPrompt {
        content: "glm profile smoke prompt".to_string(),
        hash: "glm-hash".to_string(),
        token_estimate: 7,
        template_name: "smoke".to_string(),
        template_version: "1".to_string(),
    };
    dispatcher
        .dispatch_unclaimed_analysis(
            &BeadId::from("needle-0810eb14"),
            &prompt,
            adapter_ref,
            workspace,
        )
        .await
        .expect("dispatch must complete")
}

/// The fake CLI's logged argv as a flat list.
fn fake_argv(fake: &FakeGlmClaude) -> Vec<String> {
    fs::read_to_string(&fake.log)
        .unwrap_or_else(|e| panic!("fake CLI never ran (no log at {}): {e}", fake.log.display()))
        .lines()
        .filter_map(|line| line.strip_prefix("arg="))
        .map(str::to_owned)
        .collect()
}

/// The value logged immediately after `flag`, if the flag was passed.
fn argv_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|position| args.get(position + 1))
        .map(String::as_str)
}

#[tokio::test]
async fn dispatch_success_returns_zero_and_passes_profile_flags() {
    let fake = FakeGlmClaude::new();
    let workspace = tempdir().expect("create dispatch workspace");
    let result =
        dispatch_managed("claude-code-glm-5.3-flash", &fake, workspace.path(), |_| {}).await;
    assert_eq!(result.exit_code, 0, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("\"subtype\":\"success\""),
        "stdout: {}",
        result.stdout
    );
    assert!(result.timeout_reason.is_none());

    let args = fake_argv(&fake);
    assert_eq!(argv_after(&args, "--effort"), Some("max"));
    assert_eq!(argv_after(&args, "--model"), Some("glm-5.3-flash"));
    assert!(
        args.iter()
            .any(|arg| arg == "--dangerously-skip-permissions"),
        "the shipped flags must reach the CLI verbatim: {args:?}"
    );
    assert!(
        args.iter().any(|arg| arg == "--include-partial-messages")
            && argv_after(&args, "--output-format") == Some("stream-json"),
        "the streamed-output contract must reach the CLI: {args:?}"
    );
}

#[tokio::test]
async fn dispatch_propagates_the_agents_exit_status_unmasked() {
    let fake = FakeGlmClaude::new();
    let workspace = tempdir().expect("create dispatch workspace");
    let result = dispatch_managed("claude-code-glm-5.3-flash", &fake, workspace.path(), |a| {
        a.environment
            .insert("NEEDLE_FAKE_GLM_EXIT".to_string(), "7".to_string());
    })
    .await;
    assert_eq!(
        result.exit_code, 7,
        "the agent's real exit status must reach the outcome handler \
         (this is the regression the removed `| cat` mask would hide)"
    );

    // The same exit-7 agent behind the legacy mask reports 0 — pinning why
    // the profile contract forbids the mask.
    let masked = dispatch_managed("claude-code-glm-5.3-flash", &fake, workspace.path(), |a| {
        a.environment
            .insert("NEEDLE_FAKE_GLM_EXIT".to_string(), "7".to_string());
        a.invoke_template.push_str(" | cat");
    })
    .await;
    assert_eq!(
        masked.exit_code, 0,
        "bash -c without pipefail reports the masker's status — exactly the \
         behavior the managed profile must never reintroduce"
    );
}

#[tokio::test]
async fn dispatch_captures_the_max_turns_result_subtype() {
    let fake = FakeGlmClaude::new();
    let workspace = tempdir().expect("create dispatch workspace");
    let result = dispatch_managed("claude-code-glm-5.3-flash", &fake, workspace.path(), |a| {
        a.environment
            .insert("NEEDLE_FAKE_GLM_MODE".to_string(), "max-turns".to_string());
    })
    .await;
    assert_eq!(result.exit_code, 0, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("\"subtype\":\"error_max_turns\""),
        "the max-turns result must survive into captured stdout: {}",
        result.stdout
    );
}

#[tokio::test]
async fn dispatch_partial_streaming_keeps_the_idle_deadline_alive() {
    let fake = FakeGlmClaude::new();
    let workspace = tempdir().expect("create dispatch workspace");
    // The fake emits a partial event every 0.6s for 2.4s under a 1s idle
    // deadline: only streamed partial output resetting the liveness clock
    // lets this finish instead of timing out.
    let result = dispatch_managed("claude-code-glm-5.3-flash", &fake, workspace.path(), |a| {
        a.environment.insert(
            "NEEDLE_FAKE_GLM_MODE".to_string(),
            "stream-partial".to_string(),
        );
        a.idle_timeout_secs = 1;
        a.hard_timeout_secs = 30;
    })
    .await;
    assert_eq!(
        result.exit_code, 0,
        "partial stream events must keep the attempt alive, reason: {:?}",
        result.timeout_reason
    );
    assert!(result.timeout_reason.is_none());
    for chunk in 1..=4 {
        assert!(
            result.stdout.contains(&format!("chunk-{chunk}")),
            "every partial chunk must be captured: {}",
            result.stdout
        );
    }
}

#[tokio::test]
async fn dispatch_signal_termination_preserves_shell_signal_exit_code() {
    let fake = FakeGlmClaude::new();
    let workspace = tempdir().expect("create dispatch workspace");
    let result = dispatch_managed("claude-code-glm-5.3-flash", &fake, workspace.path(), |a| {
        a.environment
            .insert("NEEDLE_FAKE_GLM_MODE".to_string(), "signal".to_string());
    })
    .await;
    assert_eq!(
        result.exit_code, 143,
        "bash encodes the fake Claude SIGTERM as 128 + SIGTERM; the dispatcher \
         must preserve that non-zero status instead of reporting success"
    );
    assert!(result.timeout_reason.is_none());
}

#[tokio::test]
async fn dispatch_idle_timeout_reports_124_with_a_structured_idle_reason() {
    let fake = FakeGlmClaude::new();
    let workspace = tempdir().expect("create dispatch workspace");
    let result = dispatch_managed("claude-code-glm-5.3-flash", &fake, workspace.path(), |a| {
        a.environment
            .insert("NEEDLE_FAKE_GLM_MODE".to_string(), "timeout".to_string());
        a.idle_timeout_secs = 1;
        a.hard_timeout_secs = 60;
    })
    .await;
    assert_eq!(result.exit_code, 124, "timeout kills report 124");
    assert_eq!(
        result.timeout_reason.as_ref().map(TimeoutReason::kind),
        Some("idle"),
        "the structured reason must name the idle deadline: {:?}",
        result.timeout_reason
    );
}

#[tokio::test]
async fn dispatch_hard_timeout_reports_124_with_a_structured_hard_reason() {
    let fake = FakeGlmClaude::new();
    let workspace = tempdir().expect("create dispatch workspace");
    let result = dispatch_managed("claude-code-glm-5.3-flash", &fake, workspace.path(), |a| {
        a.environment
            .insert("NEEDLE_FAKE_GLM_MODE".to_string(), "timeout".to_string());
        a.idle_timeout_secs = 60;
        a.hard_timeout_secs = 1;
    })
    .await;
    assert_eq!(result.exit_code, 124, "timeout kills report 124");
    assert_eq!(
        result.timeout_reason.as_ref().map(TimeoutReason::kind),
        Some("hard"),
        "the structured reason must name the hard deadline: {:?}",
        result.timeout_reason
    );
}

#[tokio::test]
async fn dispatch_1m_canary_passes_the_provider_identifier_literally() {
    let fake = FakeGlmClaude::new();
    let workspace = tempdir().expect("create dispatch workspace");
    // A file the unquoted identifier would glob to: `glm-5.3-flash[1m]` is a
    // character-class match for `glm-5.3-flash1`. If the template ever loses
    // its quoting, the logged model argument becomes this decoy's name.
    fs::write(workspace.path().join("glm-5.3-flash1"), b"decoy").expect("write glob decoy");
    let result = dispatch_managed(
        "claude-code-glm-5.3-flash-1m",
        &fake,
        workspace.path(),
        |_| {},
    )
    .await;
    assert_eq!(result.exit_code, 0, "stderr: {}", result.stderr);
    let args = fake_argv(&fake);
    assert_eq!(
        argv_after(&args, "--model"),
        Some("glm-5.3-flash[1m]"),
        "the bracketed 1M identifier must survive quoting: {args:?}"
    );
    assert_eq!(argv_after(&args, "--autocompact"), Some("1m"));
    assert!(
        args.iter()
            .any(|arg| arg == "--exclude-dynamic-system-prompt-sections")
            && args.iter().any(|arg| arg == "--no-session-persistence"),
        "the canary's evaluation flags must reach the CLI: {args:?}"
    );
}
