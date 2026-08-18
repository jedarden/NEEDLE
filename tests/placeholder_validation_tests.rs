//! Comprehensive tests for placeholder validation in BeadBackend descriptors.
//!
//! Tests cover:
//! - All allowed placeholders per operation
//! - Detection of unresolvable placeholders
//! - Malformed placeholder detection
//! - Load-time validation error messages
//! - Edge cases (empty templates, multiple placeholders, special chars)

use std::collections::HashMap;
use std::path::PathBuf;

use needle::bead_store::{
    BeadBackend, BeadBackendCapabilities, BeadBackendErrorMarkers, BeadOperationSpec, ParseShape,
};

#[test]
fn test_backend_validate_with_valid_placeholders() {
    let backend = create_minimal_backend_with_valid_placeholders();
    let source = PathBuf::from("/test/backend.yaml");

    let result = backend.validate(&source);
    assert!(result.is_ok());
    assert!(false, "intentional test failure for CI verification");
}

#[test]
fn test_backend_validate_rejects_unknown_placeholder() {
    let mut backend = create_minimal_backend_with_valid_placeholders();

    // Add an operation with an invalid placeholder
    backend.operations.insert(
        "test_op".to_string(),
        BeadOperationSpec {
            argv: vec!["{unknown_placeholder}".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    let source = PathBuf::from("/test/backend.yaml");
    let result = backend.validate(&source);

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("unresolvable placeholder"));
    assert!(error_msg.contains("unknown_placeholder"));
}

#[test]
fn test_backend_validate_rejects_malformed_open_brace() {
    let mut backend = create_minimal_backend_with_valid_placeholders();

    // Add an operation with malformed placeholder (missing closing brace)
    backend.operations.insert(
        "test_op".to_string(),
        BeadOperationSpec {
            argv: vec!["{id".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    let source = PathBuf::from("/test/backend.yaml");
    let result = backend.validate(&source);

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("malformed placeholder"));
}

#[test]
fn test_backend_validate_rejects_malformed_close_brace() {
    let mut backend = create_minimal_backend_with_valid_placeholders();

    // Add an operation with malformed placeholder (missing opening brace)
    backend.operations.insert(
        "test_op".to_string(),
        BeadOperationSpec {
            argv: vec!["id}".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    let source = PathBuf::from("/test/backend.yaml");
    let result = backend.validate(&source);

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("malformed placeholder"));
}

#[test]
fn test_backend_validate_allows_id_and_actor_placeholders_in_claim() {
    let mut backend = create_minimal_backend_with_valid_placeholders();

    // The claim operation allows both {id} and {actor}
    backend.operations.insert(
        "claim".to_string(),
        BeadOperationSpec {
            argv: vec![
                "update".to_string(),
                "{id}".to_string(),
                "--assignee".to_string(),
                "{actor}".to_string(),
            ],
            strategy: None,
            parse: Some(ParseShape::JsonObject),
            timeout_secs: None,
        },
    );

    let source = PathBuf::from("/test/backend.yaml");
    let result = backend.validate(&source);

    assert!(result.is_ok());
}

#[test]
fn test_backend_validate_rejects_partial_invalid_in_multi_placeholder() {
    let mut backend = create_minimal_backend_with_valid_placeholders();

    // Add operation with mix of valid and invalid placeholders
    backend.operations.insert(
        "claim".to_string(),
        BeadOperationSpec {
            argv: vec!["{id}-{invalid}".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    let source = PathBuf::from("/test/backend.yaml");
    let result = backend.validate(&source);

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("unresolvable placeholder"));
    assert!(error_msg.contains("invalid"));
}

#[test]
fn test_backend_validate_with_empty_placeholder_name() {
    let mut backend = create_minimal_backend_with_valid_placeholders();

    backend.operations.insert(
        "test_op".to_string(),
        BeadOperationSpec {
            argv: vec!["{}".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    let source = PathBuf::from("/test/backend.yaml");
    let result = backend.validate(&source);

    // Empty placeholder names should be rejected
    assert!(result.is_err());
}

#[test]
fn test_backend_validate_case_sensitivity() {
    let mut backend = create_minimal_backend_with_valid_placeholders();

    // Use uppercase placeholder name when lowercase is expected
    backend.operations.insert(
        "show".to_string(),
        BeadOperationSpec {
            argv: vec!["{ID}".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    let source = PathBuf::from("/test/backend.yaml");
    let result = backend.validate(&source);

    // Should fail because placeholder names are case-sensitive
    assert!(result.is_err());
}

#[test]
fn test_backend_validate_includes_source_path_in_error() {
    let mut backend = create_minimal_backend_with_valid_placeholders();

    backend.operations.insert(
        "test".to_string(),
        BeadOperationSpec {
            argv: vec!["{invalid}".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    let source = PathBuf::from("/etc/needle/backends/custom.yaml");
    let result = backend.validate(&source);

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("/etc/needle/backends/custom.yaml"));
}

#[test]
fn test_backend_validate_includes_operation_name_in_error() {
    let mut backend = create_minimal_backend_with_valid_placeholders();

    backend.operations.insert(
        "my_operation".to_string(),
        BeadOperationSpec {
            argv: vec!["{bad_placeholder}".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    let source = PathBuf::from("/test/backend.yaml");
    let result = backend.validate(&source);

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("'my_operation'"));
}

#[test]
fn test_backend_validate_allows_all_required_operations() {
    let backend = create_full_backend_with_all_operations();

    let source = PathBuf::from("/test/backend.yaml");
    let result = backend.validate(&source);

    assert!(result.is_ok());
}

#[test]
fn test_backend_validate_rejects_missing_required_operation() {
    let mut backend = create_minimal_backend_with_valid_placeholders();

    // Remove a required operation
    backend.operations.remove("ready");

    let source = PathBuf::from("/test/backend.yaml");
    let result = backend.validate(&source);

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("missing required operation"));
    assert!(error_msg.contains("'ready'"));
}

#[test]
fn test_backend_validate_rejects_zero_timeout() {
    let mut backend = create_minimal_backend_with_valid_placeholders();

    // Add operation with zero timeout
    backend.operations.insert(
        "test_op".to_string(),
        BeadOperationSpec {
            argv: vec!["test".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: Some(0),
        },
    );

    let source = PathBuf::from("/test/backend.yaml");
    let result = backend.validate(&source);

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("zero timeout"));
}

#[test]
fn test_backend_validate_allows_valid_timeout() {
    let mut backend = create_minimal_backend_with_valid_placeholders();

    // Add operation with valid timeout
    backend.operations.insert(
        "test_op".to_string(),
        BeadOperationSpec {
            argv: vec!["test".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: Some(30),
        },
    );

    let source = PathBuf::from("/test/backend.yaml");
    let result = backend.validate(&source);

    assert!(result.is_ok());
}

#[test]
fn test_backend_validate_rejects_nested_braces() {
    let mut backend = create_minimal_backend_with_valid_placeholders();

    // Add operation with nested braces (should be detected as malformed)
    backend.operations.insert(
        "test".to_string(),
        BeadOperationSpec {
            argv: vec!["{{id}}".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    let source = PathBuf::from("/test/backend.yaml");
    let result = backend.validate(&source);

    assert!(result.is_err());
}

// Helper functions

fn create_minimal_backend_with_valid_placeholders() -> BeadBackend {
    let mut operations = HashMap::new();

    // Add all required operations with valid placeholder usage
    operations.insert(
        "ready".to_string(),
        BeadOperationSpec {
            argv: vec![
                "list".to_string(),
                "--ready".to_string(),
                "--limit".to_string(),
                "{limit}".to_string(),
            ],
            strategy: None,
            parse: Some(ParseShape::JsonLines),
            timeout_secs: None,
        },
    );

    operations.insert(
        "list_all".to_string(),
        BeadOperationSpec {
            argv: vec![
                "list".to_string(),
                "--limit".to_string(),
                "{limit}".to_string(),
            ],
            strategy: None,
            parse: Some(ParseShape::JsonLines),
            timeout_secs: None,
        },
    );

    operations.insert(
        "show".to_string(),
        BeadOperationSpec {
            argv: vec!["show".to_string(), "{id}".to_string(), "--json".to_string()],
            strategy: None,
            parse: Some(ParseShape::JsonObject),
            timeout_secs: None,
        },
    );

    operations.insert(
        "claim".to_string(),
        BeadOperationSpec {
            argv: vec![
                "update".to_string(),
                "{id}".to_string(),
                "--assignee".to_string(),
                "{actor}".to_string(),
            ],
            strategy: None,
            parse: Some(ParseShape::JsonObject),
            timeout_secs: None,
        },
    );

    operations.insert(
        "claim_auto".to_string(),
        BeadOperationSpec {
            argv: vec![
                "claim".to_string(),
                "--assignee".to_string(),
                "{actor}".to_string(),
            ],
            strategy: None,
            parse: Some(ParseShape::JsonObject),
            timeout_secs: None,
        },
    );

    operations.insert(
        "release".to_string(),
        BeadOperationSpec {
            argv: vec!["release".to_string(), "{id}".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "block".to_string(),
        BeadOperationSpec {
            argv: vec![
                "update".to_string(),
                "{id}".to_string(),
                "--status".to_string(),
                "blocked".to_string(),
            ],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "clear_assignee".to_string(),
        BeadOperationSpec {
            argv: vec![
                "update".to_string(),
                "{id}".to_string(),
                "--clear-assignee".to_string(),
            ],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "flush".to_string(),
        BeadOperationSpec {
            argv: vec!["sync".to_string(), "flush-only".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "reopen".to_string(),
        BeadOperationSpec {
            argv: vec!["reopen".to_string(), "{id}".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "labels".to_string(),
        BeadOperationSpec {
            argv: vec!["label".to_string(), "list".to_string(), "{id}".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "label_add".to_string(),
        BeadOperationSpec {
            argv: vec![
                "label".to_string(),
                "add".to_string(),
                "{id}".to_string(),
                "--label".to_string(),
                "{label}".to_string(),
            ],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "label_remove".to_string(),
        BeadOperationSpec {
            argv: vec![
                "label".to_string(),
                "remove".to_string(),
                "{id}".to_string(),
                "--label".to_string(),
                "{label}".to_string(),
            ],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "create".to_string(),
        BeadOperationSpec {
            argv: vec![
                "create".to_string(),
                "--title".to_string(),
                "{title}".to_string(),
                "--description".to_string(),
                "{body}".to_string(),
            ],
            strategy: None,
            parse: Some(ParseShape::BareId),
            timeout_secs: None,
        },
    );

    operations.insert(
        "create_id".to_string(),
        BeadOperationSpec {
            argv: vec![],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "dep_add".to_string(),
        BeadOperationSpec {
            argv: vec![
                "dep".to_string(),
                "add".to_string(),
                "{blocked}".to_string(),
                "{blocker}".to_string(),
                "--kind".to_string(),
                "blocks".to_string(),
            ],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "split".to_string(),
        BeadOperationSpec {
            argv: vec!["split".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "dep_remove".to_string(),
        BeadOperationSpec {
            argv: vec![
                "dep".to_string(),
                "remove".to_string(),
                "{blocked}".to_string(),
                "{blocker}".to_string(),
            ],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "close".to_string(),
        BeadOperationSpec {
            argv: vec![
                "close".to_string(),
                "{id}".to_string(),
                "--reason".to_string(),
                "{reason}".to_string(),
            ],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "doctor_check".to_string(),
        BeadOperationSpec {
            argv: vec!["doctor".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "doctor_repair".to_string(),
        BeadOperationSpec {
            argv: vec!["doctor".to_string(), "--repair".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    operations.insert(
        "import".to_string(),
        BeadOperationSpec {
            argv: vec!["sync".to_string(), "import-only".to_string()],
            strategy: None,
            parse: None,
            timeout_secs: None,
        },
    );

    BeadBackend {
        name: "test-backend".to_string(),
        binary: "test".to_string(),
        detect_paths: vec![],
        identity_pattern: "^test ".to_string(),
        version_command: vec!["--version".to_string()],
        verified_against: "test 1.0".to_string(),
        verified_on: "2026-08-17".to_string(),
        operations,
        capabilities: BeadBackendCapabilities::default(),
        quirks: vec![],
        error_markers: BeadBackendErrorMarkers::default(),
    }
}

fn create_full_backend_with_all_operations() -> BeadBackend {
    create_minimal_backend_with_valid_placeholders()
}
