//! Comprehensive edge case and error path tests for the template system.
//!
//! This test suite covers:
//! - Edge cases: empty templates, special characters, unicode, large values
//! - Error paths: invalid placeholders, malformed templates, load failures
//! - Integration: full descriptor → render pipeline tests
//! - Performance: rendering benchmarks for various template sizes
//!
//! These tests complement the basic rendering tests in template_rendering_tests.rs
//! and the unit tests in src/template.rs.

use std::collections::HashMap;
use std::path::PathBuf;

use needle::bead_store::{
    builtin_bead_backends, load_bead_backends, BeadBackend, BeadOperationSpec,
};
use needle::template::{extract_placeholders, render, render_with_vars, RenderContext};
use tempfile::TempDir;

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_render_empty_template() {
    let context = RenderContext::default();
    let result = render("", &context);
    assert_eq!(result, "");
}

#[test]
fn test_render_template_with_only_placeholders() {
    let context = RenderContext {
        bead_id: "test-123".to_string(),
        bead_title: "Test Title".to_string(),
        ..Default::default()
    };

    let template = "{bead_id}{bead_title}";
    let result = render(template, &context);

    assert_eq!(result, "test-123Test Title");
}

#[test]
fn test_render_template_with_only_whitespace() {
    let context = RenderContext::default();
    let template = "   \n\t\n   ";
    let result = render(template, &context);
    assert_eq!(result, template);
}

#[test]
fn test_render_with_newlines_in_template() {
    let context = RenderContext {
        bead_title: "Line 1".to_string(),
        workspace: "/test".to_string(),
        ..Default::default()
    };

    let template = "Task: {bead_title}\nLocation: {workspace}\nDone";
    let result = render(template, &context);

    assert_eq!(result, "Task: Line 1\nLocation: /test\nDone");
}

#[test]
fn test_render_with_special_characters_in_values() {
    let context = RenderContext {
        bead_title: "Fix: $100 cost & 5% increase".to_string(),
        bead_body: Some(
            "Test with <html> tags, \"quotes\", 'apostrophes', and \\backslashes\\".to_string(),
        ),
        ..Default::default()
    };

    let template = "{bead_title}: {bead_body}";
    let result = render(template, &context);

    assert!(result.contains("$100 cost"));
    assert!(result.contains("5% increase"));
    assert!(result.contains("<html>"));
    assert!(result.contains("\"quotes\""));
    assert!(result.contains("'apostrophes'"));
    assert!(result.contains("\\backslashes\\"));
}

#[test]
fn test_render_with_unicode_emoji() {
    let context = RenderContext {
        bead_title: "🐛 Bug fix: 🚀 Feature ✨".to_string(),
        bead_body: Some("日本語 中文 한국ة العربية".to_string()),
        ..Default::default()
    };

    let template = "Title: {bead_title}\nBody: {bead_body}";
    let result = render(template, &context);

    assert!(result.contains("🐛"));
    assert!(result.contains("🚀"));
    assert!(result.contains("✨"));
    assert!(result.contains("日本語"));
    assert!(result.contains("中文"));
    assert!(result.contains("한국ة"));
    assert!(result.contains("العربية"));
}

#[test]
fn test_render_with_very_long_values() {
    let long_string = "x".repeat(10000);
    let context = RenderContext {
        bead_body: Some(long_string.clone()),
        ..Default::default()
    };

    let template = "{bead_body}";
    let result = render(template, &context);

    assert_eq!(result.len(), 10000);
    assert!(result.starts_with("xxx"));
    assert!(result.ends_with("xxx"));
}

#[test]
fn test_render_with_multiple_occurrences_of_same_placeholder() {
    let context = RenderContext {
        bead_id: "XYZ".to_string(),
        ..Default::default()
    };

    let template = "{bead_id} {bead_id} {bead_id} {bead_id}";
    let result = render(template, &context);

    assert_eq!(result, "XYZ XYZ XYZ XYZ");
}

#[test]
fn test_render_with_interleaved_placeholders_and_text() {
    let context = RenderContext {
        bead_id: "ABC".to_string(),
        bead_title: "Title".to_string(),
        workspace: "/path".to_string(),
        ..Default::default()
    };

    let template = "pre{id}mid{title}post{workspace}end";
    let result = render(template, &context);

    assert_eq!(result, "preABCmidTitlepost/pathend");
}

#[test]
fn test_render_with_null_bytes_in_values() {
    // Note: Rust strings don't allow null bytes, but we can test other control chars
    let context = RenderContext {
        bead_title: "Tab\there and\nnewline".to_string(),
        ..Default::default()
    };

    let template = "{bead_title}";
    let result = render(template, &context);

    assert_eq!(result, "Tab\there and\nnewline");
}

#[test]
fn test_render_with_brace_characters_in_values() {
    let context = RenderContext {
        bead_title: "Use {literal} braces [like] this (or not)".to_string(),
        ..Default::default()
    };

    let template = "{bead_title}";
    let result = render(template, &context);

    assert_eq!(result, "Use {literal} braces [like] this (or not)");
}

// ============================================================================
// Error Path Tests
// ============================================================================

#[test]
fn test_render_with_missing_placeholder_value() {
    let context = RenderContext {
        bead_title: "Test".to_string(),
        // bead_id is empty (default)
        ..Default::default()
    };

    let template = "ID: {bead_id}, Title: {bead_title}";
    let result = render(template, &context);

    // Empty string should be used for missing values
    assert_eq!(result, "ID: , Title: Test");
}

#[test]
fn test_render_with_unknown_placeholder() {
    let context = RenderContext::default();
    let template = "{unknown_placeholder}";

    // Unknown placeholders should remain as-is (not replaced)
    let result = render(template, &context);
    assert_eq!(result, "{unknown_placeholder}");
}

#[test]
fn test_extract_placehandles_from_malformed_template() {
    // Missing closing brace
    let placeholders = extract_placeholders("{bead_id");
    assert_eq!(placeholders, vec!["bead_id"]);

    // Missing opening brace
    let placeholders = extract_placeholders("bead_id}");
    assert!(placeholders.is_empty());

    // Both missing (just text)
    let placeholders = extract_placeholders("bead_id");
    assert!(placeholders.is_empty());
}

#[test]
fn test_extract_placeholders_with_nested_braces() {
    // Nested braces - only outer should be extracted
    let placeholders = extract_placeholders("{id_{inner}}");
    // Current implementation extracts "id_{inner}" as it doesn't validate nesting
    assert_eq!(placeholders, vec!["id_{inner}"]);
}

#[test]
fn test_extract_placeholders_with_special_chars() {
    let placeholders = extract_placeholders("{valid} {invalid name} {invalid-name} {123}");
    assert_eq!(placeholders, vec!["valid", "123"]);
}

#[test]
fn test_render_with_vars_empty_extra_vars() {
    let context = RenderContext::default();
    let template = "{bead_title}";
    let extra: Vec<(String, String)> = vec![];

    let result = render_with_vars(template, &context, &extra);
    assert_eq!(result, "{bead_title}"); // No value provided
}

#[test]
fn test_render_with_vars_conflicting_names() {
    let context = RenderContext {
        bead_title: "Original".to_string(),
        ..Default::default()
    };

    let template = "{bead_title}";
    let extra = vec![("bead_title".to_string(), "Overridden".to_string())];

    let result = render_with_vars(template, &context, &extra);
    // Extra vars should override context
    assert_eq!(result, "Overridden");
}

#[test]
fn test_load_backend_with_invalid_yaml() {
    let temp_dir = TempDir::new().unwrap();
    let invalid_yaml = temp_dir.path().join("invalid.yaml");

    std::fs::write(
        &invalid_yaml,
        "name: test\n  binary: bead\ninvalid: yaml: structure:\n",
    )
    .unwrap();

    let result = load_bead_backends(temp_dir.path(), &[]);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("invalid YAML") || err.contains("deser"));
}

#[test]
fn test_load_backend_with_missing_required_fields() {
    let temp_dir = TempDir::new().unwrap();
    let incomplete_yaml = temp_dir.path().join("incomplete.yaml");

    std::fs::write(&incomplete_yaml, "name: test\nbinary: bead\n").unwrap();

    let result = load_bead_backends(temp_dir.path(), &[]);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("missing required operation"));
}

#[test]
fn test_load_backend_with_empty_name() {
    let temp_dir = TempDir::new().unwrap();
    let empty_name_yaml = temp_dir.path().join("empty_name.yaml");

    std::fs::write(
        &empty_name_yaml,
        "name: \"\"\nbinary: bead\noperations: {}\n",
    )
    .unwrap();

    let result = load_bead_backends(temp_dir.path(), &[]);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("empty backend name"));
}

#[test]
fn test_load_backend_with_invalid_regex() {
    let temp_dir = TempDir::new().unwrap();
    let invalid_regex_yaml = temp_dir.path().join("invalid_regex.yaml");

    // Create a minimal valid backend with invalid regex
    let yaml = r#"
name: "test-backend"
binary: "bead"
identity_pattern: "[unclosed(regex"
verified_against: "test"
verified_on: "2024-01-01"
operations:
  ready:
    argv: []
    parse: JsonLines
"#;

    std::fs::write(&invalid_regex_yaml, yaml).unwrap();

    let result = load_bead_backends(temp_dir.path(), &[]);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("invalid identity_pattern") || err.contains("regex"));
}

#[test]
fn test_backend_validate_with_invalid_placeholders() {
    let mut backend = create_minimal_backend();
    let mut operations = std::collections::HashMap::new();
    operations.insert(
        "show".to_string(),
        BeadOperationSpec {
            argv: vec!["show".to_string(), "{invalid_placeholder}".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );
    backend.operations = operations;

    let result = backend.validate(&PathBuf::from("<test>"));
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("unresolvable placeholder"));
    assert!(err.contains("{invalid_placeholder}"));
}

#[test]
fn test_backend_validate_with_malformed_placeholder() {
    let mut backend = create_minimal_backend();
    let mut operations = std::collections::HashMap::new();
    operations.insert(
        "show".to_string(),
        BeadOperationSpec {
            argv: vec!["show".to_string(), "{unclosed".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );
    backend.operations = operations;

    let result = backend.validate(&PathBuf::from("<test>"));
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("malformed placeholder"));
}

#[test]
fn test_backend_validate_with_empty_operation_name() {
    let mut backend = create_minimal_backend();
    backend.operations.insert(
        "".to_string(),
        BeadOperationSpec {
            argv: vec![],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    // Empty operation names should be allowed (no validation on operation name itself)
    // but the operation won't match required operations
    let result = backend.validate(&PathBuf::from("<test>"));
    assert!(result.is_err());
}

// ============================================================================
// Integration Tests - Full Pipeline
// ============================================================================

#[test]
fn test_full_pipeline_render_with_bead_rs_backend() {
    let backends = builtin_bead_backends();
    let bead_rs = backends
        .iter()
        .find(|b| b.name == "bead-rs")
        .expect("bead-rs backend should exist");

    // Test the "show" operation with {id} placeholder
    let spec = bead_rs.operations.get("show").unwrap();
    let mut values = HashMap::new();
    values.insert("id", "needle-test-123");

    let rendered = render_operation_argv(&spec.argv, &values);
    assert_eq!(rendered, vec!["show", "needle-test-123", "--json"]);
}

#[test]
fn test_full_pipeline_render_with_multiple_placeholders() {
    let backends = builtin_bead_backends();
    let bead_rs = backends
        .iter()
        .find(|b| b.name == "bead-rs")
        .expect("bead-rs backend should exist");

    // Test the "claim" operation with {id} and {actor} placeholders
    let spec = bead_rs.operations.get("claim").unwrap();
    let mut values = HashMap::new();
    values.insert("id", "needle-claim-test");
    values.insert("actor", "worker-alpha");

    let rendered = render_operation_argv(&spec.argv, &values);
    assert!(rendered.contains(&"needle-claim-test".to_string()));
    assert!(rendered.contains(&"worker-alpha".to_string()));
    assert!(rendered.contains(&"in_progress".to_string()));
}

#[test]
fn test_full_pipeline_render_all_bead_rs_operations() {
    let backends = builtin_bead_backends();
    let bead_rs = backends
        .iter()
        .find(|b| b.name == "bead-rs")
        .expect("bead-rs backend should exist");

    // Test that all operations can be rendered with their allowed placeholders
    for (op_name, spec) in &bead_rs.operations {
        let values = mock_values_for_operation(op_name);
        let rendered = render_operation_argv(&spec.argv, &values);

        // Verify all placeholders were replaced
        for arg in &rendered {
            assert!(
                !arg.contains('{'),
                "Operation {} has unrendered placeholder: {}",
                op_name,
                arg
            );
            assert!(
                !arg.contains('}'),
                "Operation {} has unrendered placeholder: {}",
                op_name,
                arg
            );
        }
    }
}

#[test]
fn test_full_pipeline_load_and_render_user_backend() {
    let temp_dir = TempDir::new().unwrap();
    let user_backend = temp_dir.path().join("my-backend.yaml");

    // Create a valid user backend
    let yaml = r#"
name: "my-backend"
binary: "my-bead"
identity_pattern: "^my-bead\\s"
verified_against: "my-bead 1.0.0"
verified_on: "2024-01-01"
operations:
  ready:
    argv: ["list", "--ready", "--limit", "{limit}"]
    parse: JsonLines
  show:
    argv: ["show", "{id}", "--json"]
    parse: JsonObject
  flush:
    argv: ["flush"]
    parse: None
"#;

    std::fs::write(&user_backend, yaml).unwrap();

    let backends = load_bead_backends(temp_dir.path(), &[]).unwrap();
    let my_backend = backends.get("my-backend").expect("my-backend should load");

    // Test rendering an operation
    let spec = my_backend.operations.get("show").unwrap();
    let mut values = HashMap::new();
    values.insert("id", "test-123");

    let rendered = render_operation_argv(&spec.argv, &values);
    assert_eq!(rendered, vec!["show", "test-123", "--json"]);
}

#[test]
fn test_full_pipeline_builtin_override() {
    let temp_dir = TempDir::new().unwrap();
    let override_backend = temp_dir.path().join("bead-rs.yaml");

    // Create a user backend that overrides the builtin bead-rs
    let yaml = r#"
name: "bead-rs"
binary: "my-custom-bead"
identity_pattern: "^my-custom-bead\\s"
verified_against: "my-custom-bead 1.0.0"
verified_on: "2024-01-01"
operations:
  ready:
    argv: ["custom-ready", "--limit", "{limit}"]
    parse: JsonLines
  show:
    argv: ["custom-show", "{id}"]
    parse: JsonObject
  flush:
    argv: ["custom-flush"]
    parse: None
  claim:
    argv: ["custom-claim", "{id}", "--assignee", "{actor}"]
    parse: JsonObject
  claim_auto:
    argv: ["custom-claim-auto", "--assignee", "{actor}"]
    parse: JsonObject
  release:
    argv: ["custom-release", "{id}"]
    parse: None
  block:
    argv: ["custom-block", "{id}"]
    parse: None
  clear_assignee:
    argv: ["custom-clear", "{id}"]
    parse: None
  reopen:
    argv: ["custom-reopen", "{id}"]
    parse: None
  labels:
    argv: ["custom-labels"]
    strategy: repeated
    parse: None
  label_add:
    argv: ["custom-label-add", "{id}", "{label}"]
    parse: None
  label_remove:
    argv: ["custom-label-remove", "{id}", "{label}"]
    parse: None
  create:
    argv: ["custom-create", "--title", "{title}", "--desc", "{body}"]
    parse: BareId
  create_id:
    argv: ["custom-create-id"]
    strategy: bare_id
    parse: None
  dep_add:
    argv: ["custom-dep-add", "{blocked}", "{blocker}"]
    parse: None
  split:
    argv: ["custom-split"]
    strategy: sequential
    parse: None
  dep_remove:
    argv: ["custom-dep-remove", "{blocked}", "{blocker}"]
    parse: None
  close:
    argv: ["custom-close", "{id}", "--reason", "{reason}"]
    parse: None
  doctor_check:
    argv: ["custom-doctor"]
    parse: None
  doctor_repair:
    argv: ["custom-doctor-repair"]
    parse: None
  import:
    argv: ["custom-import"]
    strategy: input_plus_mode
    parse: None
  ref_add:
    argv: ["custom-ref-add", "{id}", "--namespace", "{namespace}", "--key", "{key}", "--value", "{value}"]
    parse: None
  ref_remove:
    argv: ["custom-ref-remove", "{id}", "--namespace", "{namespace}", "--key", "{key}"]
    parse: None
  ref_list:
    argv: ["custom-ref-list", "{id}"]
    parse: None
  ref_find:
    argv: ["custom-ref-find", "--namespace", "{namespace}", "--value", "{value}"]
    parse: JsonLines
  data_set:
    argv: ["custom-data-set", "{id}", "--key", "{key}", "--value", "{value}"]
    parse: None
  data_get:
    argv: ["custom-data-get", "{id}", "--key", "{key}"]
    parse: JsonObject
  data_list:
    argv: ["custom-data-list", "{id}"]
    parse: JsonLines
  data_remove:
    argv: ["custom-data-remove", "{id}", "--key", "{key}"]
    parse: None
  query:
    argv: ["custom-query", "{query}"]
    parse: JsonLines
  changes:
    argv: ["custom-changes", "--since", "{since}"]
    parse: JsonLines
  why:
    argv: ["custom-why", "{id}"]
    parse: JsonObject
  compare:
    argv: ["custom-compare", "{id}", "--profile", "{profile}"]
    parse: JsonObject
  recurrence_add:
    argv: ["custom-recurrence-add", "--template", "{template}", "--schedule", "{schedule}"]
    parse: None
  recurrence_remove:
    argv: ["custom-recurrence-remove", "{id}"]
    parse: None
  recurrence_list:
    argv: ["custom-recurrence-list"]
    parse: JsonLines
  policy_validate:
    argv: ["custom-policy-validate"]
    parse: JsonObject
  list_all:
    argv: ["custom-list-all", "--limit", "{limit}"]
    parse: JsonLines
"#;

    std::fs::write(&override_backend, yaml).unwrap();

    let backends = load_bead_backends(temp_dir.path(), &builtin_bead_backends()).unwrap();
    let bead_rs = backends.get("bead-rs").expect("bead-rs should exist");

    // Verify the user backend override was loaded
    assert_eq!(bead_rs.binary, "my-custom-bead");

    // Test that our custom operations render correctly
    let spec = bead_rs.operations.get("show").unwrap();
    let mut values = HashMap::new();
    values.insert("id", "override-test");

    let rendered = render_operation_argv(&spec.argv, &values);
    assert_eq!(rendered, vec!["custom-show", "override-test"]);
}

// ============================================================================
// Performance/Benchmark Tests
// ============================================================================

#[test]
fn test_performance_simple_template() {
    let context = RenderContext {
        bead_title: "Performance Test".to_string(),
        workspace: "/test/path".to_string(),
        ..Default::default()
    };

    let template = "Task: {bead_title} in {workspace}";

    let start = std::time::Instant::now();
    for _ in 0..1000 {
        let _ = render(template, &context);
    }
    let duration = start.elapsed();

    // 1000 renders should complete in reasonable time (< 100ms)
    assert!(
        duration.as_millis() < 100,
        "Rendering took too long: {:?}",
        duration
    );
}

#[test]
fn test_performance_complex_template() {
    let context = RenderContext {
        bead_id: "perf-123".to_string(),
        bead_title: "Performance Test".to_string(),
        bead_body: Some("Testing performance with longer text".to_string()),
        bead_status: "in_progress".to_string(),
        workspace: "/very/long/workspace/path/that/goes/deep/into/the/filesystem".to_string(),
        worker_id: "worker-performance-test".to_string(),
        ..Default::default()
    };

    let template = "{bead_id}|{bead_title}|{bead_body}|{bead_status}|{workspace}|{worker_id}";

    let start = std::time::Instant::now();
    for _ in 0..1000 {
        let _ = render(template, &context);
    }
    let duration = start.elapsed();

    // 1000 complex renders should complete in reasonable time (< 200ms)
    assert!(
        duration.as_millis() < 200,
        "Complex rendering took too long: {:?}",
        duration
    );
}

#[test]
fn test_performance_extract_placeholders() {
    let template =
        "{bead_id} {bead_title} {bead_body} {workspace} {worker_id} {created_at} {updated_at}";

    let start = std::time::Instant::now();
    for _ in 0..1000 {
        let _ = extract_placeholders(template);
    }
    let duration = start.elapsed();

    // 1000 extractions should complete in reasonable time (< 50ms)
    assert!(
        duration.as_millis() < 50,
        "Placeholder extraction took too long: {:?}",
        duration
    );
}

#[test]
fn test_performance_large_template() {
    let context = RenderContext {
        bead_id: "large-perf-123".to_string(),
        ..Default::default()
    };

    // Create a template with 100 placeholders
    let placeholders: Vec<String> = (1..=100).map(|i| format!("{{{}}}", i)).collect();
    let template = placeholders.join(" ");

    let start = std::time::Instant::now();
    for _ in 0..100 {
        let _ = render(&template, &context);
    }
    let duration = start.elapsed();

    // 100 renders with 100 placeholders each should complete in reasonable time (< 500ms)
    assert!(
        duration.as_millis() < 500,
        "Large template rendering took too long: {:?}",
        duration
    );
}

// ============================================================================
// Helper Functions
// ============================================================================

fn render_operation_argv(argv: &[String], values: &HashMap<&str, &str>) -> Vec<String> {
    argv.iter()
        .map(|arg| {
            let mut result = arg.clone();
            for (placeholder, value) in values {
                let pattern = format!("{{{}}}", placeholder);
                result = result.replace(&pattern, value);
            }
            result
        })
        .collect()
}

fn create_minimal_backend() -> BeadBackend {
    use needle::bead_store::{BeadBackendCapabilities, BeadBackendErrorMarkers};

    BeadBackend {
        name: "test-backend".to_string(),
        binary: "test-bead".to_string(),
        detect_paths: vec![],
        identity_pattern: "^test-bead\\s".to_string(),
        version_command: vec!["--version".to_string()],
        verified_against: "test-bead 1.0.0".to_string(),
        verified_on: "2024-01-01".to_string(),
        operations: std::collections::HashMap::new(),
        capabilities: BeadBackendCapabilities::default(),
        quirks: vec![],
        error_markers: BeadBackendErrorMarkers::default(),
    }
}

fn mock_values_for_operation(operation: &str) -> HashMap<&'static str, &'static str> {
    match operation {
        "ready" => [("limit", "10")].into_iter().collect(),
        "list_all" => [("limit", "100")].into_iter().collect(),
        "show" | "release" | "block" | "clear_assignee" | "reopen" | "labels" | "why" => {
            [("id", "test-id")].into_iter().collect()
        }
        "claim" => [("id", "test-id"), ("actor", "worker-01")]
            .into_iter()
            .collect(),
        "claim_auto" => [("actor", "worker-01")].into_iter().collect(),
        "label_add" | "label_remove" => [("id", "test-id"), ("label", "bug")].into_iter().collect(),
        "create" => [
            ("title", "Test"),
            ("body", "Description"),
            ("priority", "2"),
            ("assignee", "worker"),
            ("issue_type", "task"),
            ("labels", "test"),
        ]
        .into_iter()
        .collect(),
        "dep_add" | "dep_remove" => [("blocked", "parent"), ("blocker", "child")]
            .into_iter()
            .collect(),
        "split" => [("parent", "parent"), ("children", "child1,child2")]
            .into_iter()
            .collect(),
        "close" => [("id", "test-id"), ("reason", "Done")]
            .into_iter()
            .collect(),
        "import" => [("mode", "import-only"), ("actor", "worker")]
            .into_iter()
            .collect(),
        "compare" => [("id", "test-id"), ("profile", "native-v1")]
            .into_iter()
            .collect(),
        "query" => [("query", "status:open")].into_iter().collect(),
        "changes" => [("since", "2024-01-01")].into_iter().collect(),
        "ref_add" => [
            ("id", "test-id"),
            ("namespace", "github"),
            ("key", "issue"),
            ("value", "123"),
        ]
        .into_iter()
        .collect(),
        "ref_remove" => [("id", "test-id"), ("namespace", "github"), ("key", "issue")]
            .into_iter()
            .collect(),
        "ref_list" => [("id", "test-id")].into_iter().collect(),
        "ref_find" => [("namespace", "github"), ("value", "123")]
            .into_iter()
            .collect(),
        "data_set" => [("id", "test-id"), ("key", "key"), ("value", "value")]
            .into_iter()
            .collect(),
        "data_get" | "data_list" | "data_remove" => {
            [("id", "test-id"), ("key", "key")].into_iter().collect()
        }
        "recurrence_add" => [("template", "daily"), ("schedule", "0 9 * * *")]
            .into_iter()
            .collect(),
        "recurrence_remove" => [("id", "test-id")].into_iter().collect(),
        "recurrence_list" | "policy_validate" | "flush" | "doctor_check" | "doctor_repair"
        | "create_id" => HashMap::new(),
        _ => HashMap::new(),
    }
}
