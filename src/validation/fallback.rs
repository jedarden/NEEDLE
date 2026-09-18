//! Fallback-gate core: the verifier a gate-less workspace's own files select.
//!
//! Part 1 of the needle-66b015d6 split. Pure detection: given a workspace
//! checkout or a clean extraction of one, return the verifier to run. Nothing
//! here executes a command or touches the outcome path — wiring it into
//! `run_verification_gates` is a later split-child.
//!
//! Priority order:
//!
//! 1. `scripts/definition-of-done.sh` — the workspace declares what done
//!    means, and that outranks every marker. Interim hand-off:
//!    needle-d1b2ee0d's unified runner replaces this gate eventually.
//! 2. A language marker file, in the order the parent bead fixes them:
//!    `go.mod`, `Cargo.toml`, `package.json` (only with a non-empty `test`
//!    script), `pyproject.toml`, `pytest.ini`.
//! 3. Nothing detected — [`Verifier::NoVerifier`]. The caller decides the
//!    clean-tree + WARN path (`gate.no_verifier`); this module deliberately
//!    has no opinion on it.
//!
//! Every check is a plain filesystem read, so selection is testable without
//! executing anything.

use std::path::Path;

/// The workspace's definition-of-done script, relative to its root.
pub const DEFINITION_OF_DONE_SCRIPT: &str = "scripts/definition-of-done.sh";

/// Command run when [`Verifier::DefinitionOfDone`] is selected.
const DEFINITION_OF_DONE_COMMAND: &str = "scripts/definition-of-done.sh";

const GO_COMMAND: &str = "go build ./... && go vet ./... && go test -short ./...";
const RUST_COMMAND: &str = "cargo build --all-targets && cargo test";
const NODE_COMMAND: &str = "npm test";
const PYTHON_COMMAND: &str = "pytest -q";

/// The verifier a gate-less workspace's own files select.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verifier {
    /// `scripts/definition-of-done.sh` exists: the workspace declares what
    /// done means. Run it.
    DefinitionOfDone,
    /// A language marker file selected a verifier.
    Marker(MarkerVerifier),
    /// Nothing detected. The caller decides the clean-tree + WARN path.
    NoVerifier,
}

impl Verifier {
    /// The shell command this verifier runs, if one was selected. Run in the
    /// workspace/extraction directory it was selected from.
    pub fn command(&self) -> Option<&'static str> {
        match self {
            Verifier::DefinitionOfDone => Some(DEFINITION_OF_DONE_COMMAND),
            Verifier::Marker(marker) => Some(marker.command),
            Verifier::NoVerifier => None,
        }
    }
}

/// A verifier inferred from a build-system marker file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerVerifier {
    /// Language tag for telemetry and gate naming (`go`, `rust`, `node`,
    /// `python`).
    pub language: &'static str,
    /// The marker file that decided the selection.
    pub evidence: &'static str,
    /// Shell command to run in the workspace/extraction directory.
    pub command: &'static str,
}

/// Select the verifier for `dir` by the files it contains, in the priority
/// order of the module docs.
///
/// Marker checks are plain filesystem reads; nothing is executed here.
pub fn select_verifier(dir: &Path) -> Verifier {
    if dir.join(DEFINITION_OF_DONE_SCRIPT).is_file() {
        return Verifier::DefinitionOfDone;
    }
    if dir.join("go.mod").is_file() {
        return Verifier::Marker(MarkerVerifier {
            language: "go",
            evidence: "go.mod",
            command: GO_COMMAND,
        });
    }
    if dir.join("Cargo.toml").is_file() {
        return Verifier::Marker(MarkerVerifier {
            language: "rust",
            evidence: "Cargo.toml",
            command: RUST_COMMAND,
        });
    }
    if package_json_has_test_script(dir) {
        return Verifier::Marker(MarkerVerifier {
            language: "node",
            evidence: "package.json",
            command: NODE_COMMAND,
        });
    }
    for marker in ["pyproject.toml", "pytest.ini"] {
        if dir.join(marker).is_file() {
            return Verifier::Marker(MarkerVerifier {
                language: "python",
                evidence: marker,
                command: PYTHON_COMMAND,
            });
        }
    }
    Verifier::NoVerifier
}

/// Whether `package.json` in `dir` declares a non-empty `test` script.
///
/// A missing script — or a `package.json` that fails to parse at all, or one
/// whose `test` is not a string — is not a Node verifier: `npm test` there
/// fails or runs nothing regardless of the work, so the directory falls
/// through to the remaining markers.
fn package_json_has_test_script(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    value
        .get("scripts")
        .and_then(|scripts| scripts.get("test"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|script| !script.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `contents` to `rel` under `dir`, creating parents.
    fn write(dir: &Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn with_files(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, contents) in files {
            write(dir.path(), rel, contents);
        }
        dir
    }

    // ── Each marker branch selects its command ──

    #[test]
    fn go_mod_selects_the_go_verifier() {
        let dir = with_files(&[("go.mod", "module example.com/x\n")]);
        assert_eq!(
            select_verifier(dir.path()),
            Verifier::Marker(MarkerVerifier {
                language: "go",
                evidence: "go.mod",
                command: "go build ./... && go vet ./... && go test -short ./...",
            })
        );
    }

    #[test]
    fn cargo_toml_selects_the_rust_verifier() {
        let dir = with_files(&[("Cargo.toml", "[package]\nname = \"x\"\n")]);
        assert_eq!(
            select_verifier(dir.path()),
            Verifier::Marker(MarkerVerifier {
                language: "rust",
                evidence: "Cargo.toml",
                command: "cargo build --all-targets && cargo test",
            })
        );
    }

    #[test]
    fn package_json_with_test_script_selects_npm_test() {
        let dir = with_files(&[(
            "package.json",
            r#"{"name":"x","scripts":{"test":"vitest run"}}"#,
        )]);
        assert_eq!(
            select_verifier(dir.path()),
            Verifier::Marker(MarkerVerifier {
                language: "node",
                evidence: "package.json",
                command: "npm test",
            })
        );
    }

    #[test]
    fn pyproject_toml_selects_pytest() {
        let dir = with_files(&[("pyproject.toml", "[project]\nname = \"x\"\n")]);
        assert_eq!(
            select_verifier(dir.path()),
            Verifier::Marker(MarkerVerifier {
                language: "python",
                evidence: "pyproject.toml",
                command: "pytest -q",
            })
        );
    }

    #[test]
    fn pytest_ini_selects_pytest() {
        let dir = with_files(&[("pytest.ini", "[pytest]\n")]);
        assert_eq!(
            select_verifier(dir.path()),
            Verifier::Marker(MarkerVerifier {
                language: "python",
                evidence: "pytest.ini",
                command: "pytest -q",
            })
        );
    }

    // ── package.json without a test script falls through ──

    #[test]
    fn package_json_without_test_script_yields_no_verifier() {
        let dir = with_files(&[("package.json", r#"{"name":"x"}"#)]);
        assert_eq!(select_verifier(dir.path()), Verifier::NoVerifier);
    }

    #[test]
    fn package_json_without_test_script_falls_through_to_later_markers() {
        // No `test` script: Node does not claim the directory, so the Python
        // marker below it in the priority order still gets its chance.
        let dir = with_files(&[
            (
                "package.json",
                r#"{"name":"x","scripts":{"lint":"eslint ."}}"#,
            ),
            ("pyproject.toml", "[project]\nname = \"x\"\n"),
        ]);
        assert_eq!(
            select_verifier(dir.path()),
            Verifier::Marker(MarkerVerifier {
                language: "python",
                evidence: "pyproject.toml",
                command: "pytest -q",
            })
        );
    }

    #[test]
    fn malformed_package_json_is_not_a_node_verifier() {
        let dir = with_files(&[("package.json", "{\"name\": oops")]);
        assert_eq!(select_verifier(dir.path()), Verifier::NoVerifier);
    }

    #[test]
    fn empty_test_script_is_not_a_test_script() {
        // `npm test` against `scripts.test = ""` runs nothing; treat it as
        // absent rather than selecting a verifier that verifies nothing.
        let dir = with_files(&[("package.json", r#"{"scripts":{"test":"  "}}"#)]);
        assert_eq!(select_verifier(dir.path()), Verifier::NoVerifier);
    }

    // ── definition-of-done wins over all markers ──

    #[test]
    fn definition_of_done_outranks_every_marker() {
        let dir = with_files(&[
            ("scripts/definition-of-done.sh", "#!/bin/sh\n"),
            ("go.mod", "module example.com/x\n"),
            ("Cargo.toml", "[package]\nname = \"x\"\n"),
            ("package.json", r#"{"scripts":{"test":"vitest run"}}"#),
            ("pyproject.toml", "[project]\nname = \"x\"\n"),
            ("pytest.ini", "[pytest]\n"),
        ]);
        assert_eq!(select_verifier(dir.path()), Verifier::DefinitionOfDone);
        assert_eq!(
            select_verifier(dir.path()).command(),
            Some("scripts/definition-of-done.sh")
        );
    }

    // ── nothing detected ──

    #[test]
    fn no_markers_yield_no_verifier() {
        let dir = with_files(&[]);
        assert_eq!(select_verifier(dir.path()), Verifier::NoVerifier);
        assert_eq!(select_verifier(dir.path()).command(), None);
    }

    // ── marker order and the command helper ──

    #[test]
    fn marker_priority_follows_declared_order() {
        // The parent bead's priority order names go.mod before Cargo.toml.
        let dir = with_files(&[
            ("go.mod", "module example.com/x\n"),
            ("Cargo.toml", "[package]\n"),
        ]);
        assert_eq!(
            select_verifier(dir.path()),
            Verifier::Marker(MarkerVerifier {
                language: "go",
                evidence: "go.mod",
                command: "go build ./... && go vet ./... && go test -short ./...",
            })
        );

        let cases = [
            (
                &[
                    ("Cargo.toml", "[package]\n"),
                    ("package.json", r#"{"scripts":{"test":"npm test"}}"#),
                    ("pyproject.toml", "[project]\nname = \"x\"\n"),
                    ("pytest.ini", "[pytest]\n"),
                ][..],
                Verifier::Marker(MarkerVerifier {
                    language: "rust",
                    evidence: "Cargo.toml",
                    command: RUST_COMMAND,
                }),
            ),
            (
                &[
                    ("package.json", r#"{"scripts":{"test":"npm test"}}"#),
                    ("pyproject.toml", "[project]\nname = \"x\"\n"),
                    ("pytest.ini", "[pytest]\n"),
                ][..],
                Verifier::Marker(MarkerVerifier {
                    language: "node",
                    evidence: "package.json",
                    command: NODE_COMMAND,
                }),
            ),
            (
                &[
                    ("pyproject.toml", "[project]\nname = \"x\"\n"),
                    ("pytest.ini", "[pytest]\n"),
                ][..],
                Verifier::Marker(MarkerVerifier {
                    language: "python",
                    evidence: "pyproject.toml",
                    command: PYTHON_COMMAND,
                }),
            ),
        ];

        for (files, expected) in cases {
            let dir = with_files(files);
            assert_eq!(select_verifier(dir.path()), expected);
        }
    }

    #[test]
    fn command_helper_maps_every_variant() {
        assert_eq!(
            Verifier::DefinitionOfDone.command(),
            Some("scripts/definition-of-done.sh")
        );
        assert_eq!(
            Verifier::Marker(MarkerVerifier {
                language: "rust",
                evidence: "Cargo.toml",
                command: RUST_COMMAND,
            })
            .command(),
            Some(RUST_COMMAND)
        );
        assert_eq!(Verifier::NoVerifier.command(), None);
    }
}
