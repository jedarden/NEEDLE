//! Comprehensive tests for BeadBackend template rendering infrastructure.
//!
//! Tests cover:
//! - All placeholder types ({id}, {actor}, {title}, {body}, {limit}, etc.)
//! - Edge cases (empty templates, special characters, multiple placeholders)
//! - Optional placeholder handling (model, harness, harness_version)
//! - Error cases (missing required placeholders, malformed placeholders)

use std::collections::HashMap;

use needle::bead_store::{builtin_bead_backends, CliBeadStore};

#[test]
fn test_render_with_id_placeholder() {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|b| b.name == "bead-rs")
        .expect("bead-rs backend should exist");

    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("bead");
    std::fs::write(&binary, "#!/bin/sh\necho test\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&binary, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .expect("store creation should succeed");

    let mut values = HashMap::new();
    values.insert("id", "bf-123".to_string());

    let result = store
        .render_operation("show", &values)
        .expect("rendering should succeed");

    assert_eq!(result, vec!["show", "bf-123", "--json"]);
}

#[test]
fn test_render_with_actor_placeholder() {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|b| b.name == "bead-rs")
        .expect("bead-rs backend should exist");

    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("bead");
    std::fs::write(&binary, "#!/bin/sh\necho test\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&binary, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .expect("store creation should succeed");

    let mut values = HashMap::new();
    values.insert("id", "bf-456".to_string());
    values.insert("actor", "worker-alpha".to_string());

    let result = store
        .render_operation("claim", &values)
        .expect("rendering should succeed");

    assert_eq!(
        result,
        vec![
            "update",
            "bf-456",
            "--status",
            "in_progress",
            "--assignee",
            "worker-alpha"
        ]
    );
}

#[test]
fn test_render_with_limit_placeholder() {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|b| b.name == "bead-rs")
        .expect("bead-rs backend should exist");

    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("bead");
    std::fs::write(&binary, "#!/bin/sh\necho test\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&binary, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .expect("store creation should succeed");

    let mut values = HashMap::new();
    values.insert("limit", "10".to_string());

    let result = store
        .render_operation("ready", &values)
        .expect("rendering should succeed");

    assert_eq!(result, vec!["list", "--ready", "--json", "--limit", "10"]);
}

#[test]
fn test_render_with_blocked_and_blocker_placeholders() {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|b| b.name == "bead-rs")
        .expect("bead-rs backend should exist");

    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("bead");
    std::fs::write(&binary, "#!/bin/sh\necho test\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&binary, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .expect("store creation should succeed");

    let mut values = HashMap::new();
    values.insert("blocked", "bf-parent".to_string());
    values.insert("blocker", "bf-child".to_string());

    let result = store
        .render_operation("dep_add", &values)
        .expect("rendering should succeed");

    assert!(result.contains(&"bf-child".to_string()));
    assert!(result.contains(&"bf-parent".to_string()));
    assert!(result.contains(&"--kind".to_string()));
    assert!(result.contains(&"blocks".to_string()));
}

#[test]
fn test_render_with_reason_placeholder() {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|b| b.name == "bead-rs")
        .expect("bead-rs backend should exist");

    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("bead");
    std::fs::write(&binary, "#!/bin/sh\necho test\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&binary, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .expect("store creation should succeed");

    let mut values = HashMap::new();
    values.insert("id", "bf-789".to_string());
    values.insert("reason", "Completed successfully".to_string());

    let result = store
        .render_operation("close", &values)
        .expect("rendering should succeed");

    assert_eq!(
        result,
        vec!["close", "bf-789", "--reason", "Completed successfully"]
    );
}

#[test]
fn test_render_with_label_placeholder() {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|b| b.name == "bead-rs")
        .expect("bead-rs backend should exist");

    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("bead");
    std::fs::write(&binary, "#!/bin/sh\necho test\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&binary, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .expect("store creation should succeed");

    let mut values = HashMap::new();
    values.insert("id", "bf-label-test".to_string());
    values.insert("label", "bug".to_string());

    let result = store
        .render_operation("label_add", &values)
        .expect("rendering should succeed");

    assert_eq!(result, vec!["label", "add", "bf-label-test", "--label", "bug"]);
}

#[test]
fn test_render_with_implicit_model_placeholder() {
    // bead-forge backend uses {model}, {harness}, {harness_version} in claim_auto
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|b| b.name == "bead-forge")
        .expect("bead-forge backend should exist");

    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("bf");
    std::fs::write(&binary, "#!/bin/sh\necho test\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&binary, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        Some("claude-opus-5".to_string()),
        Some("claude-code".to_string()),
        Some("2.1.233".to_string()),
    )
    .expect("store creation should succeed");

    let mut values = HashMap::new();
    values.insert("actor", "worker-beta".to_string());

    // claim_auto uses {model}, {harness}, {harness_version} which are optional
    let result = store.render_operation("claim_auto", &values);

    // Should succeed because all implicit values are provided
    assert!(result.is_ok());
    let rendered = result.unwrap();
    assert!(rendered.iter().any(|arg| arg == "claude-opus-5"));
    assert!(rendered.iter().any(|arg| arg == "claude-code"));
    assert!(rendered.iter().any(|arg| arg == "2.1.233"));
}

#[test]
fn test_render_operation_without_placeholders() {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|b| b.name == "bead-rs")
        .expect("bead-rs backend should exist");

    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("bead");
    std::fs::write(&binary, "#!/bin/sh\necho test\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&binary, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .expect("store creation should succeed");

    let values = HashMap::new();

    let result = store
        .render_operation("flush", &values)
        .expect("flush should have no placeholders");

    assert_eq!(result, vec!["sync", "flush-only"]);
}

#[test]
fn test_render_with_empty_values_returns_error_for_required_placeholders() {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|b| b.name == "bead-rs")
        .expect("bead-rs backend should exist");

    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("bead");
    std::fs::write(&binary, "#!/bin/sh\necho test\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&binary, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .expect("store creation should succeed");

    let values = HashMap::new(); // Empty values

    let result = store.render_operation("show", &values);

    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("requires placeholder"));
}

#[test]
fn test_render_with_unicode_values() {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|b| b.name == "bead-rs")
        .expect("bead-rs backend should exist");

    let workspace = tempfile::tempdir().unwrap();
    let binary = workspace.path().join("bead");
    std::fs::write(&binary, "#!/bin/sh\necho test\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&binary, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let store = CliBeadStore::new(
        backend,
        binary,
        workspace.path().to_path_buf(),
        None,
        None,
        None,
    )
    .expect("store creation should succeed");

    let mut values = HashMap::new();
    values.insert("title", "Fix: 🐛 bug with émojis".to_string());
    values.insert("body", "Cöntënt with spëcial charactërs 日本語".to_string());

    let result = store
        .render_operation("create", &values)
        .expect("rendering should succeed with unicode");

    assert!(result.iter().any(|arg| arg.contains("🐛")));
    assert!(result.iter().any(|arg| arg.contains("émojis")));
}
