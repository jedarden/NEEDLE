//! Source-level regression guard for in-process Explore isolation.
//!
//! `ExploreStrand::new` immediately discovers workspaces when its configured
//! workspace list is empty. A test that feeds `Config::default()` into a
//! `Worker` or `StrandRunner` can therefore scan the operator's real HOME even
//! if it never polls the worker. This lint reads Rust sources only: it neither
//! opens nor mutates a bead store.

use std::path::{Path, PathBuf};

#[derive(Debug, Eq, PartialEq)]
struct Violation {
    function: String,
    line: usize,
}

#[test]
fn lint_in_process_worker_tests_isolate_explore() {
    assert_classifier_contract();

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();

    for directory in ["src", "tests"] {
        for file in rust_files(&root.join(directory)) {
            let source = std::fs::read_to_string(&file)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", file.display()));
            for violation in find_violations(&source) {
                offenders.push(format!(
                    "{}:{}: `{}` constructs an Explore-capable worker from a default config",
                    file.strip_prefix(root).unwrap_or(&file).display(),
                    violation.line,
                    violation.function,
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "unsafe in-process test configuration can scan the operator's real HOME.\n\
         Before constructing Worker, StrandRunner, or ExploreStrand from a default config,\n\
         set `config.strands.explore.workspace_root` to a tempfile::TempDir path, select\n\
         explicit test workspaces, or set `config.strands.explore.enabled = false`.\n\
         Keep the TempDir alive for the whole test. Offenders:\n{}",
        offenders.join("\n")
    );
}

fn assert_classifier_contract() {
    let unsafe_source = r#"
        async fn new_worker_test() {
            let config = Config::default();
            let worker = Worker::new(config, "fixture".to_string(), store);
            worker.run_cycle().await.unwrap();
        }
    "#;
    assert_eq!(
        find_violations(unsafe_source),
        vec![Violation {
            function: "new_worker_test".to_string(),
            line: 2,
        }],
        "a newly introduced default-Config Worker test must fail the guard"
    );

    let isolated_source = r#"
        fn isolated_runner_test() {
            let root = tempfile::tempdir().unwrap();
            let mut config = Config::default();
            config.strands.explore.workspace_root = root.path().to_path_buf();
            config.strands.explore.workspaces.clear();
            let runner = StrandRunner::from_config(&config, "fixture", registry, telemetry);
        }
    "#;
    assert!(find_violations(isolated_source).is_empty());

    let too_late_source = r#"
        fn late_isolation_does_not_help() {
            let mut config = Config::default();
            let worker = Worker::new(config, "fixture".to_string(), store);
            worker.config.strands.explore.workspace_root = root.path().to_path_buf();
        }
    "#;
    assert_eq!(
        find_violations(too_late_source).len(),
        1,
        "isolation must be configured before Worker constructs Explore"
    );

    let component_only_source = r#"
        fn health_config_test() {
            let config = Config::default();
            let monitor = HealthMonitor::new(&config.health);
        }
    "#;
    assert!(
        find_violations(component_only_source).is_empty(),
        "a component that cannot reach Explore must not be forced to carry fake isolation"
    );
}

fn find_violations(source: &str) -> Vec<Violation> {
    let code = mask_comments_and_literals(source);
    function_bodies(&code)
        .into_iter()
        .filter_map(|function| {
            let body = &code[function.body_start..function.body_end];
            let full_default = path_call_position(body, "Config", "default").is_some();
            let explore_default = path_call_position(body, "ExploreConfig", "default").is_some();
            let worker_at = path_call_position(body, "Worker", "new")
                .into_iter()
                .chain(path_call_position(body, "StrandRunner", "from_config"))
                .min();
            let explore_at = path_call_position(body, "ExploreStrand", "new");
            let constructs_worker = worker_at.is_some();
            let constructs_explore = explore_at.is_some();

            if !(full_default && (constructs_worker || constructs_explore)
                || explore_default && constructs_explore)
            {
                return None;
            }

            // Isolation applied after construction is too late:
            // ExploreStrand::new has already auto-discovered the default root.
            let constructor_at = worker_at.into_iter().chain(explore_at).min().unwrap();
            let compact: String = body[..constructor_at]
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            let root_is_pinned = if full_default {
                compact.contains("strands.explore.workspace_root=")
                    || compact.contains("strands.explore.workspace_root:")
            } else {
                compact.contains(".workspace_root=") || compact.contains("workspace_root:")
            };
            let explore_is_disabled = if full_default {
                compact.contains("strands.explore.enabled=false")
            } else {
                compact.contains(".enabled=false")
                    || compact.contains("ExploreConfig{enabled:false")
            };
            let workspaces_are_pinned = if full_default {
                has_nonempty_workspace_selection(&compact, "strands.explore.workspaces")
            } else {
                has_nonempty_workspace_selection(&compact, ".workspaces")
                    || has_nonempty_workspace_field(&compact)
            };

            (!root_is_pinned && !explore_is_disabled && !workspaces_are_pinned).then(|| Violation {
                function: function.name,
                line: code[..function.start]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    + 1,
            })
        })
        .collect()
}

fn has_nonempty_workspace_selection(compact: &str, field: &str) -> bool {
    if compact.contains(&format!("{field}.push(")) {
        return true;
    }
    let pattern = format!("{field}=vec![");
    compact
        .match_indices(&pattern)
        .any(|(start, _)| compact.as_bytes().get(start + pattern.len()).copied() != Some(b']'))
}

fn has_nonempty_workspace_field(compact: &str) -> bool {
    let pattern = "workspaces:vec![";
    compact
        .match_indices(pattern)
        .any(|(start, _)| compact.as_bytes().get(start + pattern.len()).copied() != Some(b']'))
}

fn path_call_position(source: &str, type_name: &str, method: &str) -> Option<usize> {
    let needle = format!("{type_name}::{method}");
    source.match_indices(&needle).find_map(|(start, _)| {
        let before_is_identifier = start
            .checked_sub(1)
            .and_then(|index| source.as_bytes().get(index))
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
        let after = &source[start + needle.len()..];
        (!before_is_identifier && after.trim_start().starts_with('(')).then_some(start)
    })
}

struct FunctionBody {
    name: String,
    start: usize,
    body_start: usize,
    body_end: usize,
}

fn function_bodies(source: &str) -> Vec<FunctionBody> {
    let bytes = source.as_bytes();
    let mut functions = Vec::new();
    let mut cursor = 0;

    while cursor + 2 <= bytes.len() {
        if &bytes[cursor..cursor + 2] != b"fn"
            || cursor
                .checked_sub(1)
                .and_then(|index| bytes.get(index))
                .is_some_and(|byte| is_identifier_byte(*byte))
            || bytes
                .get(cursor + 2)
                .is_some_and(|byte| is_identifier_byte(*byte))
        {
            cursor += 1;
            continue;
        }

        let mut name_start = cursor + 2;
        while bytes
            .get(name_start)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            name_start += 1;
        }
        let mut name_end = name_start;
        while bytes
            .get(name_end)
            .is_some_and(|byte| is_identifier_byte(*byte))
        {
            name_end += 1;
        }
        if name_start == name_end {
            cursor += 2;
            continue;
        }

        let mut open = name_end;
        while let Some(byte) = bytes.get(open) {
            if *byte == b'{' || *byte == b';' {
                break;
            }
            open += 1;
        }
        if bytes.get(open) != Some(&b'{') {
            cursor = open.saturating_add(1);
            continue;
        }

        let mut depth = 1usize;
        let mut close = open + 1;
        while close < bytes.len() && depth > 0 {
            match bytes[close] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            close += 1;
        }
        if depth != 0 {
            break;
        }

        functions.push(FunctionBody {
            name: source[name_start..name_end].to_string(),
            start: cursor,
            body_start: open + 1,
            body_end: close - 1,
        });
        cursor = close;
    }

    functions
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn mask_comments_and_literals(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut masked = bytes.to_vec();
    let mut cursor = 0;

    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(b"//") {
            let end = bytes[cursor..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |offset| cursor + offset);
            blank_non_newlines(&mut masked[cursor..end]);
            cursor = end;
        } else if bytes[cursor..].starts_with(b"/*") {
            let start = cursor;
            cursor += 2;
            let mut depth = 1usize;
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
            blank_non_newlines(&mut masked[start..cursor]);
        } else if let Some(end) = raw_string_end(bytes, cursor) {
            blank_non_newlines(&mut masked[cursor..end]);
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
            blank_non_newlines(&mut masked[start..cursor]);
        } else if bytes[cursor] == b'\'' {
            if let Some(end) = char_literal_end(source, cursor) {
                blank_non_newlines(&mut masked[cursor..end]);
                cursor = end;
            } else {
                cursor += 1;
            }
        } else {
            cursor += 1;
        }
    }

    String::from_utf8(masked).expect("masking valid UTF-8 with ASCII spaces preserves UTF-8")
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

fn char_literal_end(source: &str, start: usize) -> Option<usize> {
    let tail = source.get(start + 1..)?;
    let mut characters = tail.char_indices();
    let (_, first) = characters.next()?;
    if first == '\\' {
        let mut escaped = false;
        for (offset, character) in tail.char_indices().skip(1) {
            if character == '\'' && !escaped {
                return Some(start + 1 + offset + 1);
            }
            escaped = character == '\\' && !escaped;
            if character != '\\' {
                escaped = false;
            }
        }
        None
    } else {
        let closing = start + 1 + first.len_utf8();
        (source.as_bytes().get(closing) == Some(&b'\'')).then_some(closing + 1)
    }
}

fn blank_non_newlines(bytes: &mut [u8]) {
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
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // `autotests = false`; this directory contains parked sources
                // that are not reachable from any declared Cargo test target.
                if path.file_name().is_some_and(|name| name == "pending") {
                    continue;
                }
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }

    files.sort();
    files
}
