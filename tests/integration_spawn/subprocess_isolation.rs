//! Static enforcement for subprocess test isolation.
//!
//! A temporary HOME is not sufficient when a test inherits an Explore root
//! from a workspace config or environment. This guard checks every direct
//! `needle` subprocess constructor in the test sources and requires both
//! overrides on the same function. The shared `IsolatedChildEnv` helper is
//! checked separately by its runtime regression test in `isolation.rs`.
//!
//! Constructor detection is alias-aware: `use tokio::process::Command as
//! WorkerCommand;` followed by `WorkerCommand::new(...)` is scanned exactly
//! like a plain `Command::new(...)`, so an import alias cannot hide a launch
//! from this audit. The negative self-tests below pin that behavior — if the
//! detector ever stops recognizing a launch shape, they fail instead of the
//! fleet's guard silently passing everything.

use std::path::{Path, PathBuf};

#[test]
fn needle_subprocesses_pin_home_and_explore_root() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();

    for file in rust_files(&root.join("tests")) {
        let source = std::fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", file.display()));
        let masked = mask_non_code(&source);
        let label = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .display()
            .to_string();
        violations.extend(audit_source(&label, &source, &masked));
    }

    assert!(
        violations.is_empty(),
        "unsafe needle subprocess test configuration detected:\n{}",
        violations.join("\n")
    );
}

#[test]
fn guard_recognizes_literal_and_aliased_needle_launches() {
    let synthetic = r#"
        use tokio::process::Command as WorkerCommand;
        fn literal() {
            let _ = Command::new("needle");
        }
        fn aliased() {
            let binary = needle_binary_path();
            let _ = Command::new(binary);
        }
        fn worker() {
            let _ = WorkerCommand::new(needle_binary_path());
        }
    "#;
    let synthetic_masked = mask_non_code(synthetic);
    assert_eq!(
        subprocess_constructors(synthetic, &synthetic_masked).len(),
        3,
        "the audit must recognize literal, aliased-path, and import-aliased NEEDLE launches"
    );

    let unrelated = r#"
        fn other() {
            let _ = TempDir::new();
        }
    "#;
    assert_eq!(
        subprocess_constructors(unrelated, &mask_non_code(unrelated)).len(),
        0,
        "constructors of unrelated types must not be audited"
    );

    let helper_indirection = r#"
        fn current_candidate() -> PathBuf {
            PathBuf::from(env!("CARGO_BIN_EXE_needle"))
        }
        fn upgrade_fixture() {
            let _ = Command::new(current_candidate())
                .env("HOME", home.path())
                .env("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT", home.path());
        }
    "#;
    assert_eq!(
        subprocess_constructors(helper_indirection, &mask_non_code(helper_indirection)).len(),
        1,
        "the audit must recognize a compiled NEEDLE path returned by a helper"
    );
}

#[test]
fn guard_rejects_a_launch_without_isolation_overrides() {
    let unisolated = r#"
        fn bare_launch() {
            let output = Command::new(needle_binary_path())
                .args(["version"])
                .output();
        }
    "#;
    let violations = audit_source("synthetic.rs", unisolated, &mask_non_code(unisolated));
    assert_eq!(
        violations.len(),
        1,
        "an unpinned launch must be flagged: {violations:?}"
    );
    assert!(
        violations[0].contains("must set HOME and pin or disable Explore"),
        "the violation must name both missing overrides: {}",
        violations[0]
    );
}

#[test]
fn guard_accepts_pinned_and_disabled_explore_variants() {
    let pinned_root = r#"
        fn pinned() {
            let mut command = Command::new(needle_binary_path());
            command
                .env("HOME", home.path())
                .env("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT", home.path());
        }
    "#;
    assert!(
        audit_source("synthetic.rs", pinned_root, &mask_non_code(pinned_root)).is_empty(),
        "a launch pinning HOME and the Explore root must pass"
    );

    let disabled = r#"
        fn disabled() {
            let mut command = WorkerCommand::new(needle_binary_path());
            command
                .env("HOME", home.path())
                .env("NEEDLE_STRANDS__EXPLORE__ENABLED", "false");
        }
    "#;
    assert!(
        audit_source("synthetic.rs", disabled, &mask_non_code(disabled)).is_empty(),
        "a launch disabling Explore must pass"
    );
}

#[test]
fn guard_rejects_an_import_aliased_launch_without_overrides() {
    // The alias alone must not hide the launch: `Command as WorkerCommand`
    // puts `WorkerCommand::new` under the same audit as `Command::new`.
    let aliased = r#"
        use tokio::process::Command as WorkerCommand;

        fn aliased_bare_launch() {
            let output = WorkerCommand::new(needle_binary_path())
                .args(["version"])
                .output();
        }
    "#;
    let violations = audit_source("synthetic.rs", aliased, &mask_non_code(aliased));
    assert_eq!(
        violations.len(),
        1,
        "an import-aliased unpinned launch must still be flagged: {violations:?}"
    );
}

/// Audit one source file: every needle subprocess constructor must pin HOME
/// and pin or disable Explore within the same function.
fn audit_source(label: &str, source: &str, masked: &str) -> Vec<String> {
    subprocess_constructors(source, masked)
        .into_iter()
        .filter_map(|(offset, constructor)| {
            let body = function_source(source, masked, offset);
            let has_home = body.contains(".env(\"HOME\"");
            let has_explore_root = body.contains("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT")
                || body.contains(".env(\"NEEDLE_STRANDS__EXPLORE__ENABLED\", \"false\")")
                || body.contains("enabled: false");
            if has_home && has_explore_root {
                return None;
            }
            let line = line_number(source, offset);
            Some(format!(
                "{label}:{line}: `{constructor}` must set HOME and pin or disable Explore"
            ))
        })
        .collect()
}

/// Every `Command`-family constructor site in `source`: `Command::new` plus
/// any `use ...::Command as <Alias>` import alias's `<Alias>::new`.
fn command_aliases(masked: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = masked[cursor..].find("Command as ") {
        let name_start = cursor + relative;
        let alias_start = name_start + "Command as ".len();
        // The pattern must begin an identifier: `SubCommand as X` must not
        // register `X` for the wrong type.
        let begins_identifier = name_start == 0
            || !masked
                .as_bytes()
                .get(name_start - 1)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
        if begins_identifier {
            let alias = masked[alias_start..]
                .chars()
                .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
                .collect::<String>();
            if !alias.is_empty() && alias != "Command" && !aliases.contains(&alias) {
                aliases.push(alias);
            }
        }
        cursor = alias_start;
    }
    aliases
}

fn subprocess_constructors(source: &str, masked: &str) -> Vec<(usize, String)> {
    let aliases = command_aliases(masked);
    let mut constructors = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = masked[cursor..].find("::new(") {
        let open = cursor + relative;
        // Walk back over the type name so `TokioCommand::new(` is attributed
        // to its own identifier rather than to the `Command::new(` substring.
        let name_end = open;
        let name_start = masked[..name_end]
            .char_indices()
            .rev()
            .take_while(|(_, character)| character.is_ascii_alphanumeric() || *character == '_')
            .map(|(index, _)| index)
            .last()
            .unwrap_or(name_end);
        cursor = open + "::new(".len();
        if name_start == name_end {
            continue;
        }
        let type_name = &masked[name_start..name_end];
        if type_name != "Command" && !aliases.iter().any(|alias| alias == type_name) {
            continue;
        }
        let argument_end = matching_delimiter(masked, open + "::new(".len() - 1, b'(', b')');
        let argument = &source[open + "::new(".len() - 1..argument_end];
        let function = function_source(source, masked, name_start);
        let is_transform = argument.contains("needle_transform");
        let direct_needle_path = argument.contains("CARGO_BIN_EXE_needle")
            || argument.contains("NEXTEST_BIN_EXE_needle")
            || argument.contains("needle_binary")
            || argument.contains("needle_path")
            || argument.contains("current_candidate")
            || argument.contains("&needle")
            || argument.contains("needle)")
            || argument.contains("\"needle\"");
        // Also cover an alias such as `let binary = needle_binary_path();`
        // followed by `Command::new(binary)`. A function that resolves the
        // NEEDLE binary and constructs a process command must isolate that
        // command even when the path is not passed inline.
        let function_resolves_needle = function.contains("CARGO_BIN_EXE_needle")
            || function.contains("NEXTEST_BIN_EXE_needle")
            || function.contains("needle_binary_path")
            || function.contains("needle_binary")
            || function.contains("needle_path")
            || function.contains("current_candidate");
        // A cargo-based launch is also a real NEEDLE subprocess even though
        // its constructor argument is `cargo`, not the binary path itself.
        let cargo_runs_needle = argument.contains("\"cargo\"")
            && function.contains("--bin")
            && function.contains("\"needle\"");
        if !is_transform && (direct_needle_path || function_resolves_needle || cargo_runs_needle) {
            constructors.push((name_start, format!("{type_name}::new")));
        }
    }

    constructors
}

fn function_source<'a>(source: &'a str, masked: &str, offset: usize) -> &'a str {
    let function_start = masked[..offset]
        .rfind("fn ")
        .unwrap_or_else(|| panic!("needle constructor is outside a function"));
    let open = masked[function_start..offset]
        .find('{')
        .map(|relative| function_start + relative)
        .unwrap_or_else(|| panic!("function has no body"));
    let close = matching_delimiter(masked, open, b'{', b'}');
    &source[function_start..close]
}

fn matching_delimiter(source: &str, open: usize, opening: u8, closing: u8) -> usize {
    let bytes = source.as_bytes();
    assert_eq!(bytes.get(open), Some(&opening));
    let mut depth = 0;
    for (index, byte) in bytes.iter().enumerate().skip(open) {
        if *byte == opening {
            depth += 1;
        } else if *byte == closing {
            depth -= 1;
            if depth == 0 {
                return index + 1;
            }
        }
    }
    panic!("unterminated delimiter in test source");
}

fn line_number(source: &str, offset: usize) -> usize {
    source[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn mask_non_code(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut masked = bytes.to_vec();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(b"//") {
            let end = bytes[cursor..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |relative| cursor + relative);
            blank(&mut masked[cursor..end]);
            cursor = end;
        } else if bytes[cursor..].starts_with(b"/*") {
            let start = cursor;
            cursor += 2;
            let mut depth = 1;
            while cursor < bytes.len() && depth > 0 {
                if bytes[cursor..].starts_with(b"/*") {
                    depth += 1;
                    cursor += 2;
                } else if bytes[cursor..].starts_with(b"*/") {
                    depth -= 1;
                    cursor += 2;
                } else {
                    cursor += 1;
                }
            }
            blank(&mut masked[start..cursor]);
        } else if let Some(end) = raw_string_end(bytes, cursor) {
            blank(&mut masked[cursor..end]);
            cursor = end;
        } else if bytes[cursor] == b'"' {
            let start = cursor;
            cursor += 1;
            while cursor < bytes.len() {
                if bytes[cursor] == b'\\' {
                    cursor = (cursor + 2).min(bytes.len());
                } else {
                    let closing = bytes[cursor] == b'"';
                    cursor += 1;
                    if closing {
                        break;
                    }
                }
            }
            blank(&mut masked[start..cursor]);
        } else if bytes[cursor] == b'\'' {
            let start = cursor;
            cursor += 1;
            while cursor < bytes.len() {
                if bytes[cursor] == b'\\' {
                    cursor = (cursor + 2).min(bytes.len());
                } else {
                    let closing = bytes[cursor] == b'\'';
                    cursor += 1;
                    if closing {
                        break;
                    }
                }
            }
            blank(&mut masked[start..cursor]);
        } else {
            cursor += 1;
        }
    }
    String::from_utf8(masked).expect("source is valid UTF-8")
}

fn raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hash_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    let hashes = cursor - hash_start;
    cursor += 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && bytes.get(cursor + 1..cursor + 1 + hashes)
                == Some(&bytes[hash_start..hash_start + hashes])
        {
            return Some(cursor + 1 + hashes);
        }
        cursor += 1;
    }
    Some(bytes.len())
}

fn blank(bytes: &mut [u8]) {
    for byte in bytes {
        if *byte != b'\n' && *byte != b'\r' {
            *byte = b' ';
        }
    }
}

fn rust_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", current.display()))
        {
            let entry = entry.expect("read test source directory entry");
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}
