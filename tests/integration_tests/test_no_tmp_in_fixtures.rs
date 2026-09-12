//! Lint test: no test fixture may name a shared directory as its bead
//! workspace.
//!
//! On 2026-08-30 unit tests passed a literal root-of-tmp path as `workspace`,
//! which drove the HOOP event/heartbeat taps and trace capture to create
//! `/tmp/.beads/` on the needle-ci pod and on ex44. bead-rs 0.2.x discovery
//! stops at the first `.beads` it meets walking upward, so every later
//! `bead init` in a temp dir under /tmp failed with "No workspace found:
//! discovery stopped at /tmp/.beads" — 40 of 48 integration tests red.
//!
//! Every fixture now roots its paths in a `tempfile::tempdir()`, and
//! production code that needs a HOME-less fallback uses
//! `std::env::temp_dir()`. This test keeps the tree that way: it fails on any
//! return of the banned expression anywhere under `src/` or `tests/`.
//!
//! The needle is assembled from string pieces so this file never contains the
//! literal it bans — the definition of done for the fixture sweep greps the
//! whole tree for exactly that string and must stay empty.

use std::path::{Path, PathBuf};

/// The banned expression, split so this source file does not contain it.
const BANNED_EXPR: &str = concat!("PathBuf", "::from(\"/tmp\")");

#[test]
fn lint_no_pathbuf_from_tmp_anywhere_in_the_tree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders: Vec<String> = Vec::new();

    for dir in ["src", "tests"] {
        for file in rust_files(&root.join(dir)) {
            let content = match std::fs::read_to_string(&file) {
                Ok(c) => c,
                Err(e) => panic!("failed to read {}: {e}", file.display()),
            };
            for (i, line) in content.lines().enumerate() {
                if line.contains(BANNED_EXPR) {
                    offenders.push(format!(
                        "{}:{}: {}",
                        file.strip_prefix(root).unwrap_or(&file).display(),
                        i + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "test fixtures must not name a shared directory as a bead workspace;\n\
         every fixture should root its paths in tempfile::tempdir() (and\n\
         production HOME-less fallbacks in std::env::temp_dir()). Offenders:\n{}",
        offenders.join("\n")
    );
}

/// Collect every `.rs` file under `dir`, recursively, in sorted order so the
/// failure output is deterministic.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue, // missing src/ or tests/ is not this lint's concern
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}
