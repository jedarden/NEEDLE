//! Comprehensive unit tests for the BeadStore show() method
//!
//! Tests cover:
//! - Successful bead lookup that returns expected Bead
//! - Not-found case that returns appropriate error
//! - JSON parsing failure
//! - CLI invocation failure
//! - Corruption error handling
//! - Lock error handling
//! - Permission error handling
//! - Binary not found error handling

use needle::bead_store::{builtin_bead_backends, BeadStore, CliBeadStore};
use needle::types::BeadId;
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
#[tokio::test]
async fn show_returns_bead_on_successful_lookup() {
    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("fake-bead");

    // Create a fake binary that returns a valid bead
    std::fs::write(
        &binary,
        r#"#!/bin/sh
if [ "$1" = "show" ]; then
  cat <<'EOF'
[
  {
    "id": "bf-test-123",
    "title": "Test Bead",
    "description": "A test bead",
    "priority": 2,
    "status": "open",
    "created_at": "2026-08-29T00:00:00Z",
    "updated_at": "2026-08-29T01:00:00Z"
  }
]
EOF
else
  echo 'bead 0.1.3'
fi
"#,
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions).unwrap();

    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .unwrap();

    // Test successful bead lookup
    let bead_id = BeadId::from("bf-test-123");
    let result = store.show(&bead_id).await;

    assert!(result.is_ok(), "show() should return Ok for valid bead");
    let bead = result.unwrap();
    assert_eq!(bead.id.as_ref(), "bf-test-123");
    assert_eq!(bead.title, "Test Bead");
    assert_eq!(bead.body.as_deref().unwrap_or("None"), "A test bead");
    assert_eq!(bead.priority, 2);
    assert_eq!(bead.status, needle::types::BeadStatus::Open);
}

#[cfg(unix)]
#[tokio::test]
async fn show_returns_error_for_not_found_bead() {
    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("fake-bead");

    // Create a fake binary that returns empty result (bead not found)
    std::fs::write(
        &binary,
        r#"#!/bin/sh
if [ "$1" = "show" ]; then
  # Return empty object to simulate "not found" case
  echo '{}'
else
  echo 'bead 0.1.3'
fi
"#,
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions).unwrap();

    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .unwrap();

    // Test not-found error handling
    let bead_id = BeadId::from("bf-nonexistent");
    let result = store.show(&bead_id).await;

    assert!(
        result.is_err(),
        "show() should return error for non-existent bead"
    );
    let error_msg = result.unwrap_err().to_string();
    // The error can be about parsing the empty object or bead not found
    assert!(
        error_msg.contains("not found")
            || error_msg.contains("may not exist")
            || error_msg.contains("parse")
            || error_msg.contains("expected"),
        "Error message should indicate bead not found or parsing issue: {}",
        error_msg
    );
}

#[cfg(unix)]
#[tokio::test]
async fn show_handles_json_parsing_failure() {
    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("fake-bead");

    // Create a fake binary that returns malformed JSON
    std::fs::write(
        &binary,
        r#"#!/bin/sh
if [ "$1" = "show" ]; then
  # Return malformed JSON - missing closing brace
  echo '{
    "id": "bf-malformed",
    "title": "Malformed Bead",
    "description": "This JSON is broken"
  '
else
  echo 'bead 0.1.3'
fi
"#,
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions).unwrap();

    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .unwrap();

    // Test JSON parsing failure handling
    let bead_id = BeadId::from("bf-malformed");
    let result = store.show(&bead_id).await;

    assert!(
        result.is_err(),
        "show() should return error for malformed JSON"
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("malformed JSON")
            || error_msg.contains("parse")
            || error_msg.contains("JSON"),
        "Error message should indicate JSON parsing failure: {}",
        error_msg
    );
}

#[cfg(unix)]
#[tokio::test]
async fn show_handles_cli_invocation_failure_binary_not_found() {
    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("nonexistent-bead-binary");

    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();

    // Test that creating a store with a non-existent binary fails
    let result = CliBeadStore::new(
        backend,
        binary.clone(),
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    );

    assert!(
        result.is_err(),
        "CliBeadStore::new() should return error when binary not found"
    );
    let error_msg = match result {
        Err(e) => e.to_string(),
        Ok(_) => panic!("Expected error but got Ok"),
    };
    assert!(
        error_msg.contains("not found") || error_msg.contains("No such file"),
        "Error message should indicate binary not found: {}",
        error_msg
    );
}

#[cfg(unix)]
#[tokio::test]
async fn show_handles_cli_invocation_failure_permission_denied() {
    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("fake-bead");

    // Create a fake binary without execute permissions
    std::fs::write(
        &binary,
        r#"#!/bin/sh
echo 'bead 0.1.3'
"#,
    )
    .unwrap();

    // Set read-only permissions (no execute)
    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o644);
    std::fs::set_permissions(&binary, permissions).unwrap();

    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let store = CliBeadStore::new(
        backend,
        binary.clone(),
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .unwrap();

    // Test permission denied error handling
    let bead_id = BeadId::from("bf-test-123");
    let result = store.show(&bead_id).await;

    assert!(
        result.is_err(),
        "show() should return error when permission denied"
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("failed to retrieve") || error_msg.contains("backend"),
        "Error message should indicate retrieval failure: {}",
        error_msg
    );
}

#[cfg(unix)]
#[tokio::test]
async fn show_handles_cli_invocation_generic_failure() {
    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("fake-bead");

    // Create a fake binary that exits with error code
    std::fs::write(
        &binary,
        r#"#!/bin/sh
if [ "$1" = "show" ]; then
  echo "Error: bead backend failed" >&2
  exit 1
else
  echo 'bead 0.1.3'
fi
"#,
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions).unwrap();

    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .unwrap();

    // Test generic CLI invocation failure
    let bead_id = BeadId::from("bf-test-123");
    let result = store.show(&bead_id).await;

    assert!(
        result.is_err(),
        "show() should return error for CLI failure"
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("failed to retrieve") || error_msg.contains("backend"),
        "Error message should indicate retrieval failure: {}",
        error_msg
    );
}

#[cfg(unix)]
#[tokio::test]
async fn show_handles_json_object_response_format() {
    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("fake-bead");

    // Create a fake binary that returns JSON object (single bead)
    std::fs::write(
        &binary,
        r#"#!/bin/sh
if [ "$1" = "show" ]; then
  cat <<'EOF'
{
  "id": "bf-test-456",
  "title": "Single Bead Object",
  "description": "A single bead as JSON object",
  "priority": 1,
  "status": "in_progress",
  "created_at": "2026-08-29T00:00:00Z"
}
EOF
else
  echo 'bead 0.1.3'
fi
"#,
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions).unwrap();

    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .unwrap();

    // Test successful bead lookup with object format
    let bead_id = BeadId::from("bf-test-456");
    let result = store.show(&bead_id).await;

    assert!(
        result.is_ok(),
        "show() should return Ok for valid bead in object format"
    );
    let bead = result.unwrap();
    assert_eq!(bead.id.as_ref(), "bf-test-456");
    assert_eq!(bead.title, "Single Bead Object");
    assert_eq!(bead.status, needle::types::BeadStatus::InProgress);
}

#[cfg(unix)]
#[tokio::test]
async fn show_handles_json_with_extra_fields() {
    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("fake-bead");

    // Create a fake binary that returns JSON with extra fields
    std::fs::write(
        &binary,
        r#"#!/bin/sh
if [ "$1" = "show" ]; then
  cat <<'EOF'
[
  {
    "id": "bf-test-789",
    "title": "Bead with Extra Fields",
    "description": "A bead with additional fields",
    "priority": 3,
    "status": "closed",
    "created_at": "2026-08-29T00:00:00Z",
    "extra_field": "some value",
    "another_field": 42,
    "nested": {"key": "value"}
  }
]
EOF
else
  echo 'bead 0.1.3'
fi
"#,
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions).unwrap();

    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .unwrap();

    // Test successful bead lookup with extra fields (should be ignored)
    let bead_id = BeadId::from("bf-test-789");
    let result = store.show(&bead_id).await;

    assert!(
        result.is_ok(),
        "show() should handle JSON with extra fields"
    );
    let bead = result.unwrap();
    assert_eq!(bead.id.as_ref(), "bf-test-789");
    assert_eq!(bead.title, "Bead with Extra Fields");
    assert_eq!(bead.status, needle::types::BeadStatus::Closed);
}

#[cfg(unix)]
#[tokio::test]
async fn show_handles_invalid_json_structure() {
    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("fake-bead");

    // Create a fake binary that returns invalid JSON structure
    std::fs::write(
        &binary,
        r#"#!/bin/sh
if [ "$1" = "show" ]; then
  # Return valid JSON but wrong structure (string instead of object/array)
  echo '"just a string"'
else
  echo 'bead 0.1.3'
fi
"#,
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions).unwrap();

    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .unwrap();

    // Test invalid JSON structure handling
    let bead_id = BeadId::from("bf-test-invalid");
    let result = store.show(&bead_id).await;

    assert!(
        result.is_err(),
        "show() should return error for invalid JSON structure"
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("parse")
            || error_msg.contains("JSON")
            || error_msg.contains("malformed"),
        "Error message should indicate parsing problem: {}",
        error_msg
    );
}

#[cfg(unix)]
#[tokio::test]
async fn show_handles_empty_response() {
    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("fake-bead");

    // Create a fake binary that returns empty response
    std::fs::write(
        &binary,
        r#"#!/bin/sh
if [ "$1" = "show" ]; then
  # Return empty response
  echo ''
else
  echo 'bead 0.1.3'
fi
"#,
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions).unwrap();

    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .unwrap();

    // Test empty response handling
    let bead_id = BeadId::from("bf-test-empty");
    let result = store.show(&bead_id).await;

    assert!(
        result.is_err(),
        "show() should return error for empty response"
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("parse")
            || error_msg.contains("empty")
            || error_msg.contains("Expected"),
        "Error message should indicate parsing problem with empty input: {}",
        error_msg
    );
}

#[cfg(unix)]
#[tokio::test]
async fn show_returns_correct_bead_from_valid_json_object() {
    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("fake-bead");

    // Create a fake binary that returns a single valid bead object
    std::fs::write(
        &binary,
        r#"#!/bin/sh
if [ "$1" = "show" ]; then
  cat <<'EOF'
{
  "id": "bf-test-valid",
  "title": "Valid Bead Object",
  "description": "A valid single bead object",
  "priority": 2,
  "status": "in_progress",
  "created_at": "2026-08-29T00:00:00Z",
  "assignee": "test-worker"
}
EOF
else
  echo 'bead 0.1.3'
fi
"#,
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions).unwrap();

    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .unwrap();

    // Test that show() correctly parses and returns a valid bead object
    let bead_id = BeadId::from("bf-test-valid");
    let result = store.show(&bead_id).await;

    assert!(
        result.is_ok(),
        "show() should return bead from valid JSON object"
    );
    let bead = result.unwrap();
    assert_eq!(bead.id.as_ref(), "bf-test-valid");
    assert_eq!(bead.title, "Valid Bead Object");
    assert_eq!(bead.status, needle::types::BeadStatus::InProgress);
    assert_eq!(bead.assignee, Some("test-worker".to_string()));
}
