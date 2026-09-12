//! Comprehensive tests for placeholder validation in BeadBackend descriptors.
//!
//! Tests cover:
//! - All allowed placeholders per operation
//! - Detection of unresolvable placeholders
//! - Malformed placeholder detection
//! - Load-time validation error messages
//! - Edge cases (empty templates, multiple placeholders, special chars)

use std::path::PathBuf;

use needle::bead_store::{builtin_bead_backends, BeadBackend, BeadOperationSpec, ParseShape};

#[test]
fn test_backend_validate_with_valid_placeholders() {
    let backend = create_minimal_backend_with_valid_placeholders();
    let source = PathBuf::from("/test/backend.yaml");

    let result = backend.validate(&source);
    assert!(result.is_ok());
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
    builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .expect("bead-rs backend should exist")
}

fn create_full_backend_with_all_operations() -> BeadBackend {
    create_minimal_backend_with_valid_placeholders()
}
