//! Language-default verification gates for workspaces that declare none.
//!
//! Plan section 4.4 step 6. A workspace with no `gates:` in `.needle.yaml`
//! used to run no verification at all, so an agent's exit 0 became
//! `verified_success` in the ledger. On 2026-09-12 that was 74 of 79
//! workspaces on the host. Here the workspace's own build files decide the
//! gate: a Rust crate is checked, a Go module vetted and built. Python and
//! Node workspaces get a gate only when `validation.default_gates.python` /
//! `.node` names a command that works in a clean extraction — one without a
//! virtualenv or `node_modules`, where the builtin `pytest` / `npm test`
//! failed every single time on 2026-09-12 (25 false failures in eleven
//! hours, two workspaces degraded).
//!
//! The builtin commands are the cheap deterministic checks, not the full
//! suites: gates run in a clean extraction of committed state under
//! `validation.outcome_timeout_seconds`, and a cold full `cargo test` of a
//! large crate would time out into a gate execution error and degrade the
//! workspace. `validation.default_gates.<language>` overrides the commands.
//!
//! An explicit `gates: []` (or `verification: []`) in `.needle.yaml` opts a
//! workspace out; a declared gate list always wins.

use std::path::Path;

use crate::config::DefaultGatesConfig;

/// A default gate derived from the workspace's build files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedGate {
    /// Language tag, also the gate name suffix (`default_rust`).
    pub language: &'static str,
    /// What decided it (`Cargo.toml`, `package.json` …).
    pub evidence: String,
    /// Shell commands, run in order in a clean extraction.
    pub commands: Vec<String>,
}

impl DetectedGate {
    /// The gate name as it appears in gate reports and the ledger.
    pub fn gate_name(&self) -> String {
        format!("default_{}", self.language)
    }
}

/// Detect the default gate for `root`, if its build files name a language.
///
/// Detection order matters only for polyglot roots: the primary build system
/// wins (Rust before Go before Python before Node), mirroring how the fleet's
/// repositories are laid out (a Rust service with a `package.json` for its
/// dashboard is a Rust workspace).
pub fn detect(root: &Path, config: &DefaultGatesConfig) -> Option<DetectedGate> {
    if !config.enabled {
        return None;
    }
    if root.join("Cargo.toml").is_file() {
        return Some(DetectedGate {
            language: "rust",
            evidence: "Cargo.toml".into(),
            commands: pick(&config.rust, || {
                vec!["cargo check --all-targets --quiet".into()]
            }),
        });
    }
    if root.join("go.mod").is_file() {
        return Some(DetectedGate {
            language: "go",
            evidence: "go.mod".into(),
            commands: pick(&config.go, || {
                vec!["go vet ./...".into(), "go build ./...".into()]
            }),
        });
    }
    // Python and Node have no builtin: a clean extraction of committed state
    // carries no virtualenv and no node_modules, so `pytest` and `npm test`
    // fail there every time regardless of the work. On 2026-09-12 the
    // builtins produced 25 false gate failures in the first eleven hours and
    // degraded two workspaces (needle-c4b0a0d1 follow-up). A repository that
    // wants a Python or Node default declares the command that works in its
    // extraction via `validation.default_gates.python` / `.node` (typically
    // one that installs its dependencies first, or a byte-compile step).
    for marker in [
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "requirements.txt",
    ] {
        if root.join(marker).is_file() {
            if config.python.is_empty() {
                return None;
            }
            return Some(DetectedGate {
                language: "python",
                evidence: marker.into(),
                commands: config.python.clone(),
            });
        }
    }
    if let Some(script) = package_json_test_script(root) {
        if !is_npm_placeholder(&script) {
            if config.node.is_empty() {
                return None;
            }
            return Some(DetectedGate {
                language: "node",
                evidence: format!("package.json scripts.test = {script:?}"),
                commands: config.node.clone(),
            });
        }
    }
    None
}

fn pick(configured: &[String], builtin: impl FnOnce() -> Vec<String>) -> Vec<String> {
    if configured.is_empty() {
        builtin()
    } else {
        configured.to_vec()
    }
}

fn package_json_test_script(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("scripts")?
        .get("test")?
        .as_str()
        .map(str::to_string)
}

/// `npm init` writes a test script that only prints an error and exits 1.
fn is_npm_placeholder(script: &str) -> bool {
    let s = script.trim();
    s.is_empty() || (s.contains("no test specified") && s.contains("exit 1"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DefaultGatesConfig {
        DefaultGatesConfig::default()
    }

    #[test]
    fn rust_workspace_gets_cargo_check() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let gate = detect(dir.path(), &cfg()).expect("rust");
        assert_eq!(gate.language, "rust");
        assert_eq!(gate.gate_name(), "default_rust");
        assert_eq!(
            gate.commands,
            vec!["cargo check --all-targets --quiet".to_string()]
        );
    }

    #[test]
    fn rust_wins_over_a_dashboard_package_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"vitest run"}}"#,
        )
        .unwrap();
        assert_eq!(detect(dir.path(), &cfg()).unwrap().language, "rust");
    }

    #[test]
    fn python_has_no_builtin_but_honours_a_configured_command() {
        // A clean extraction has no virtualenv: pytest would fail every time,
        // so nothing runs unless the operator says what works there.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        assert!(detect(dir.path(), &cfg()).is_none());
        let configured = DefaultGatesConfig {
            python: vec!["python3 -m compileall -q .".into()],
            ..DefaultGatesConfig::default()
        };
        let gate = detect(dir.path(), &configured).unwrap();
        assert_eq!(gate.language, "python");
        assert_eq!(gate.gate_name(), "default_python");
        assert_eq!(gate.commands, configured.python);
    }

    #[test]
    fn node_has_no_builtin_and_ignores_the_npm_placeholder_script() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#,
        )
        .unwrap();
        let configured = DefaultGatesConfig {
            node: vec!["npm ci --silent && npm test --silent".into()],
            ..DefaultGatesConfig::default()
        };
        // Placeholder script: no gate even when a command is configured.
        assert!(detect(dir.path(), &configured).is_none());
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"vitest run"}}"#,
        )
        .unwrap();
        // Real script, no configured command: still no builtin (no node_modules
        // in a clean extraction).
        assert!(detect(dir.path(), &cfg()).is_none());
        let gate = detect(dir.path(), &configured).unwrap();
        assert_eq!(gate.language, "node");
        assert_eq!(gate.commands, configured.node);
    }

    #[test]
    fn go_module_is_vetted_and_built() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module x\n").unwrap();
        let gate = detect(dir.path(), &cfg()).unwrap();
        assert_eq!(gate.language, "go");
        assert_eq!(gate.commands.len(), 2);
    }

    #[test]
    fn unknown_layout_and_disabled_config_yield_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect(dir.path(), &cfg()).is_none());
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let off = DefaultGatesConfig {
            enabled: false,
            ..DefaultGatesConfig::default()
        };
        assert!(detect(dir.path(), &off).is_none());
    }

    #[test]
    fn configured_commands_override_the_builtin() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let custom = DefaultGatesConfig {
            rust: vec!["cargo test --quiet".into()],
            ..DefaultGatesConfig::default()
        };
        assert_eq!(
            detect(dir.path(), &custom).unwrap().commands,
            vec!["cargo test --quiet".to_string()]
        );
    }
}
