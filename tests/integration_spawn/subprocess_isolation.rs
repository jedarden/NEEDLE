//! Static enforcement for subprocess test isolation.
//!
//! A temporary HOME is not sufficient when a test inherits an Explore root
//! from a workspace config or environment. This guard checks every direct
//! `needle` subprocess constructor in the test sources and requires both
//! overrides on the same function. The shared `IsolatedChildEnv` helper is
//! checked separately by its runtime regression test in `isolation.rs`.

use std::path::{Path, PathBuf};

#[test]
fn needle_subprocesses_pin_home_and_explore_root() {
    let synthetic = r#"
        fn literal() {
            let _ = Command::new("needle");
        }
        fn aliased() {
            let binary = needle_binary_path();
            let _ = Command::new(binary);
        }
    "#;
    let synthetic_masked = mask_non_code(synthetic);
    assert_eq!(
        needle_constructors(synthetic, &synthetic_masked).len(),
        2,
        "the audit must recognize literal and aliased NEEDLE launches"
    );

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();

    for file in rust_files(&root.join("tests")) {
        let source = std::fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", file.display()));
        let masked = mask_non_code(&source);
        for (offset, function) in needle_constructors(&source, &masked) {
            let body = function_source(&source, &masked, offset);
            let has_home = body.contains(".env(\"HOME\"");
            let has_explore_root = body.contains("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT")
                || body.contains(".env(\"NEEDLE_STRANDS__EXPLORE__ENABLED\", \"false\")")
                || body.contains("enabled: false");
            if !has_home || !has_explore_root {
                violations.push(format!(
                    "{}:{}: `{function}` must set HOME and pin or disable Explore",
                    file.strip_prefix(root).unwrap_or(&file).display(),
                    line_number(&source, offset),
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "unsafe needle subprocess test configuration detected:\n{}",
        violations.join("\n")
    );
}

fn needle_constructors(source: &str, masked: &str) -> Vec<(usize, String)> {
    let mut constructors = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = masked[cursor..].find("Command::new(") {
        let start = cursor + relative;
        let open = start + "Command::new".len();
        let end = matching_delimiter(masked, open, b'(', b')');
        let argument = &source[open..end];
        let function = function_source(source, masked, start);
        let is_transform = argument.contains("needle_transform");
        let direct_needle_path = argument.contains("CARGO_BIN_EXE_needle")
            || argument.contains("NEXTEST_BIN_EXE_needle")
            || argument.contains("needle_binary")
            || argument.contains("needle_path")
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
            || function.contains("needle_path");
        // A cargo-based launch is also a real NEEDLE subprocess even though
        // its constructor argument is `cargo`, not the binary path itself.
        let cargo_runs_needle = argument.contains("\"cargo\"")
            && function.contains("--bin")
            && function.contains("\"needle\"");
        if !is_transform && (direct_needle_path || function_resolves_needle || cargo_runs_needle) {
            constructors.push((start, "Command::new".to_string()));
        }
        cursor = end;
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
