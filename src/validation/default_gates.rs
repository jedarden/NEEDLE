//! Language-default verification gates for workspaces that declare none.
//!
//! Plan section 4.4 step 6. A workspace with no `gates:` in `.needle.yaml`
//! used to run no verification at all, so an agent's exit 0 became
//! `verified_success` in the ledger. On 2026-09-12 that was 74 of 79
//! workspaces on the host. Here the workspace's own build files decide the
//! gate: a Rust crate is checked, a Go module vetted and built, a Python
//! project's tests run when it has any (byte-compiled otherwise), a Node
//! package's real `test` script run.
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
    for marker in [
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "requirements.txt",
    ] {
        if root.join(marker).is_file() {
            let has_tests = root.join("tests").is_dir()
                || root.join("test").is_dir()
                || root.join("pytest.ini").is_file()
                || root.join("conftest.py").is_file()
                || pyproject_mentions_pytest(root);
            let builtin = if has_tests {
                vec!["python3 -m pytest -q -x -p no:cacheprovider".into()]
            } else {
                vec!["python3 -m compileall -q .".into()]
            };
            return Some(DetectedGate {
                language: "python",
                evidence: marker.into(),
                commands: pick(&config.python, || builtin),
            });
        }
    }
    if let Some(script) = package_json_test_script(root) {
        if !is_npm_placeholder(&script) {
            return Some(DetectedGate {
                language: "node",
                evidence: format!("package.json scripts.test = {script:?}"),
                commands: pick(&config.node, || vec!["npm test --silent".into()]),
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

fn pyproject_mentions_pytest(root: &Path) -> bool {
    std::fs::read_to_string(root.join("pyproject.toml"))
        .map(|text| text.contains("[tool.pytest") || text.contains("pytest"))
        .unwrap_or(false)
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
    fn python_with_tests_runs_pytest_and_without_byte_compiles() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        let gate = detect(dir.path(), &cfg()).unwrap();
        assert_eq!(gate.language, "python");
        assert!(
            gate.commands[0].contains("compileall"),
            "{:?}",
            gate.commands
        );
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        let gate = detect(dir.path(), &cfg()).unwrap();
        assert!(gate.commands[0].contains("pytest"), "{:?}", gate.commands);
    }

    #[test]
    fn node_needs_a_real_test_script() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#,
        )
        .unwrap();
        assert!(detect(dir.path(), &cfg()).is_none());
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"vitest run"}}"#,
        )
        .unwrap();
        let gate = detect(dir.path(), &cfg()).unwrap();
        assert_eq!(gate.language, "node");
        assert_eq!(gate.commands, vec!["npm test --silent".to_string()]);
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
