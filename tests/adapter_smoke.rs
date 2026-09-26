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
use needle::dispatch::{builtin_adapters, test_agent, AgentAdapter, AgentTestStatus};
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
