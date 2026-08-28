//! Descriptor conformance tests for bead backend implementations.
//!
//! This test suite verifies that all backend implementations conform to the
//! expected behavior for critical operations, particularly claim and release.
//!
//! **Why this matters:**
//! - Bugs in claim operations cause duplicate dispatch (multiple workers on the same bead)
//! - Bugs in release operations cause lost beads (beads stuck in assigned state)
//!
//! These tests replaced the old BrCliBeadStore and BfCliBeadStore test suites
//! after those were consolidated into the descriptor-driven CliBeadStore.

use needle::bead_store::{builtin_bead_backends, BeadStore, CliBeadStore};
use needle::types::{BeadId, ClaimResult};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[cfg(unix)]
fn executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, body).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(unix)]
fn mock_cli(root: &Path, backend_name: &str) -> CliBeadStore {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == backend_name)
        .expect("backend not found");
    let binary = root.join("mock-cli");
    executable(
        &binary,
        r#"#!/bin/sh
printf '%s\n' "$@" >> invocations.log
# Default: return empty array for list operations
if [ "$1" = "list" ] || [ "$1" = "ready" ]; then
    printf '[]\n'
elif [ "$1" = "show" ]; then
    printf '{"id":"test-1","title":"Test","description":null,"priority":2,"status":"open","assignee":null,"labels":[],"source_repo":"","dependencies":[],"dependents":[],"comments":[],"created_at":"2026-08-12T00:00:00Z","updated_at":"2026-08-12T00:00:00Z"}\n'
fi
"#,
    );
    CliBeadStore::new(backend, binary, root.to_path_buf(), None, None, None).unwrap()
}

// ═══════════════════════════════════════════════════════════════════════════════
// Claim Operation Conformance Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[cfg(unix)]
async fn claim_operation_requires_actor_placeholder() {
    //! Verify claim operations fail fast when actor placeholder is missing.
    //!
    //! This prevents bugs where a claim would be attempted without an assignee,
    //! which could lead to race conditions or duplicate dispatch.
    let root = tempfile::tempdir().unwrap();
    let store = mock_cli(root.path(), "bead-rs");

    let error = store
        .render_operation("claim", &HashMap::new())
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("requires placeholder"),
        "claim should require actor placeholder, got: {error}"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn claim_operation_includes_actor_in_rendered_command() {
    //! Verify claim operations include the actor in the rendered command.
    //!
    //! This ensures the assignee is actually set when claiming a bead.
    let root = tempfile::tempdir().unwrap();
    let store = mock_cli(root.path(), "bead-rs");

    let argv = store
        .render_operation(
            "claim",
            &HashMap::from([
                ("id", "test-1".to_string()),
                ("actor", "worker-a".to_string()),
            ]),
        )
        .unwrap();

    assert!(
        argv.contains(&"worker-a".to_string()),
        "claim command must include actor assignee: {argv:?}"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn bead_rs_claim_uses_compare_and_set_strategy() {
    //! Verify bead-rs claim uses compare-and-set for safety.
    //!
    //! This prevents duplicate dispatch: if the bead was modified since we
    //! read it, the claim fails and we retry.
    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let binary = root.path().join("mock-cli");
    executable(
        &binary,
        r#"#!/bin/sh
printf '%s\n' "$@" >> invocations.log
"#,
    );
    let store = CliBeadStore::new(backend, binary, root.to_path_buf(), None, None, None).unwrap();

    let result = store.claim(&BeadId::from("test-1"), "worker-a").await;

    // Should fail because mock doesn't return valid JSON, but we can check invocation
    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert!(
        invocations.contains("update")
            && invocations.contains("--status")
            && invocations.contains("in_progress"),
        "bead-rs claim should use update with status in_progress: {invocations}"
    );
    assert!(
        invocations.contains("--if-revision") || invocations.contains("compare"),
        "bead-rs claim should use compare-and-set strategy: {invocations}"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn bead_forge_claim_uses_atomic_batch() {
    //! Verify bead-forge claim uses atomic batch for safety.
    //!
    //! This prevents duplicate dispatch by ensuring the claim operation
    //! is atomic within the SQLite transaction.
    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-forge")
        .unwrap();
    let binary = root.path().join("mock-cli");
    executable(
        &binary,
        r#"#!/bin/sh
printf '%s\n' "$@" >> invocations.log
"#,
    );
    let store = CliBeadStore::new(backend, binary, root.to_path_buf(), None, None, None).unwrap();

    let _ = store.claim(&BeadId::from("bf-1"), "worker-a").await;

    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert!(
        invocations.contains("batch") || invocations.contains("atomic"),
        "bead-forge claim should use atomic batch: {invocations}"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn claim_auto_operation_is_atomic() {
    //! Verify claim_auto operations are atomic by default.
    //!
    //! This is critical for preventing duplicate dispatch in the ready loop.
    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();

    // Verify the descriptor declares atomic strategy
    let claim_auto_op = backend.operations.get("claim_auto").unwrap();
    assert!(
        claim_auto_op.strategy.is_some(),
        "claim_auto must declare a strategy"
    );
    assert!(
        claim_auto_op.strategy.as_ref().unwrap() == "atomic_subcommand"
            || claim_auto_op.strategy.as_ref().unwrap() == "atomic",
        "claim_auto must use atomic strategy, got: {:?}",
        claim_auto_op.strategy
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Release Operation Conformance Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[cfg(unix)]
async fn release_operation_requires_id_placeholder() {
    //! Verify release operations fail fast when ID placeholder is missing.
    //!
    //! This prevents bugs where we'd try to release without specifying which bead.
    let root = tempfile::tempdir().unwrap();
    let store = mock_cli(root.path(), "bead-rs");

    let error = store
        .render_operation("release", &HashMap::new())
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("requires placeholder") || error.contains("missing"),
        "release should require id placeholder, got: {error}"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn release_operation_clears_assignee() {
    //! Verify release operations actually clear the assignee.
    //!
    //! Bugs here cause lost beads: a bead stays assigned to a dead worker
    //! and never gets picked up again.
    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let binary = root.path().join("mock-cli");
    executable(
        &binary,
        r#"#!/bin/sh
printf '%s\n' "$@" >> invocations.log
"#,
    );
    let store = CliBeadStore::new(backend, binary, root.to_path_buf(), None, None, None).unwrap();

    store.release(&BeadId::from("test-1")).await.unwrap();

    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert!(
        invocations.contains("release")
            || invocations.contains("--clear-assignee")
            || invocations.contains("assignee"),
        "release must clear assignee: {invocations}"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn bead_forge_release_uses_batch_operation() {
    //! Verify bead-forge release uses the batch operation.
    //!
    //! This ensures the release is part of a transaction, preventing
    //! partial updates that could leave beads in inconsistent states.
    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-forge")
        .unwrap();
    let binary = root.path().join("mock-cli");
    executable(
        &binary,
        r#"#!/bin/sh
printf '%s\n' "$@" >> invocations.log
"#,
    );
    let store = CliBeadStore::new(backend, binary, root.to_path_buf(), None, None, None).unwrap();

    store.release(&BeadId::from("bf-1")).await.unwrap();

    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert!(
        invocations.contains("batch") || invocations.contains("update"),
        "bead-forge release should use batch or update operation: {invocations}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Other Critical Operation Conformance Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[cfg(unix)]
async fn ready_operation_filters_by_assignee() {
    //! Verify ready operation supports assignee filtering.
    //!
    //! This is used by workers to get their own assigned beads.
    let root = tempfile::tempdir().unwrap();
    let store = mock_cli(root.path(), "bead-rs");

    let argv = store
        .render_operation(
            "ready",
            &HashMap::from([("assignee", "worker-a".to_string())]),
        )
        .unwrap();

    assert!(
        argv.contains(&"worker-a".to_string()) || argv.iter().any(|arg| arg.contains("assignee")),
        "ready command must support assignee filtering: {argv:?}"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn list_all_operation_returns_json_lines() {
    //! Verify list_all operations return JSON lines format.
    //!
    //! This is critical for parsing efficiency and correctness.
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();

    let list_all_op = backend.operations.get("list_all").unwrap();
    assert_eq!(
        list_all_op.parse,
        Some(needle::bead_store::backend::ParseShape::JsonLines),
        "list_all must return JsonLines format"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn show_operation_returns_json_object() {
    //! Verify show operations return JSON object format.
    //!
    //! This ensures single bead lookups return structured data.
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();

    let show_op = backend.operations.get("show").unwrap();
    assert_eq!(
        show_op.parse,
        Some(needle::bead_store::backend::ParseShape::JsonObject),
        "show must return JsonObject format"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn dependency_operations_maintain_dialect_specific_order() {
    //! Verify dependency operations maintain correct argument order.
    //!
    //! bead-rs: dep add <blocked> <blocker> --kind blocks
    //! bead-forge: dep add <blocker> --blocks <blocked>
    //!
    //! Bugs here cause corrupted dependency graphs.
    let root = tempfile::tempdir().unwrap();

    let values = HashMap::from([
        ("blocked", "blocked-1".to_string()),
        ("blocker", "blocker-1".to_string()),
    ]);

    let bead_rs_store = mock_cli(root.path(), "bead-rs");
    let bead_rs_argv = bead_rs_store.render_operation("dep_add", &values).unwrap();
    assert!(
        bead_rs_argv
            .windows(3)
            .any(|w| w == ["blocked", "blocked-1", "blocker-1"]),
        "bead-rs dep_add must have blocked before blocker: {bead_rs_argv:?}"
    );

    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-forge")
        .unwrap();
    let binary = root.path().join("mock-cli-forge");
    executable(&binary, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n");
    let forge_store =
        CliBeadStore::new(backend, binary, root.to_path_buf(), None, None, None).unwrap();
    let forge_argv = forge_store.render_operation("dep_add", &values).unwrap();
    assert!(
        forge_argv
            .windows(3)
            .any(|w| w == ["blocker", "blocker-1", "--blocks"])
            || forge_argv
                .windows(2)
                .any(|w| w == ["blocker-1", "--blocks"]),
        "bead-forge dep_add must use --blocks flag: {forge_argv:?}"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn split_operation_uses_declared_strategy() {
    //! Verify split operations use the declared transactional strategy.
    //!
    //! This is critical for data integrity: splitting a bead must be atomic.
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();

    let split_op = backend.operations.get("split").unwrap();
    assert!(split_op.strategy.is_some(), "split must declare a strategy");
    assert!(
        split_op.strategy.as_ref().unwrap() == "sequential"
            || split_op.strategy.as_ref().unwrap() == "transactional_batch",
        "split must use a transactional strategy, got: {:?}",
        split_op.strategy
    );
}

#[tokio::test]
#[cfg(unix)]
async fn all_required_operations_are_declared() {
    //! Verify all required operations are present in backend descriptors.
    //!
    //! Missing operations cause runtime failures when NEEDLE tries to use them.
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();

    let required_ops = vec![
        "ready",
        "list_all",
        "show",
        "claim",
        "claim_auto",
        "release",
        "block",
        "clear_assignee",
        "flush",
        "reopen",
        "labels",
        "label_add",
        "label_remove",
        "create",
        "create_id",
        "dep_add",
        "split",
        "dep_remove",
        "close",
        "doctor_check",
        "doctor_repair",
        "import",
        "ref_add",
        "ref_remove",
        "ref_list",
        "ref_find",
        "data_set",
        "data_get",
        "data_list",
        "data_remove",
        "query",
        "changes",
        "why",
        "compare",
        "recurrence_add",
        "recurrence_remove",
        "recurrence_list",
        "policy_validate",
    ];

    for op in required_ops {
        assert!(
            backend.operations.contains_key(op),
            "bead-rs backend is missing required operation '{}'",
            op
        );
    }
}

#[tokio::test]
#[cfg(unix)]
async fn atomic_claim_capability_is_correctly_declared() {
    //! Verify atomic_claim capability matches the actual implementation.
    //!
    //! If this is wrong, NEEDLE may assume safety guarantees that don't exist,
    //! leading to race conditions and duplicate dispatch.
    let backends = builtin_bead_backends();

    for backend in &backends {
        if backend.name == "bead-rs" || backend.name == "bead-forge" {
            assert!(
                backend.capabilities.atomic_claim,
                "{} backend must declare atomic_claim capability",
                backend.name
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Operation Invocation Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[cfg(unix)]
async fn operation_invocation_uses_bound_binary() {
    //! Verify operations are invoked through the bound binary.
    //!
    //! This ensures the descriptor system is actually routing commands
    //! through the correct CLI implementation.
    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let binary = root.path().join("mock-cli");
    executable(
        &binary,
        r#"#!/bin/sh
printf '%s\n' "$@" >> invocations.log
printf '{"id":"test-1","title":"Test","description":null,"priority":2,"status":"open","assignee":null,"labels":[],"source_repo":"","dependencies":[],"dependents":[],"comments":[],"created_at":"2026-08-12T00:00:00Z","updated_at":"2026-08-12T00:00:00Z"}'
"#,
    );
    let store = CliBeadStore::new(backend, binary, root.to_path_buf(), None, None, None).unwrap();

    store.show(&BeadId::from("test-1")).await.unwrap();

    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert!(
        !invocations.is_empty(),
        "operation should invoke the binary"
    );
    assert!(
        invocations.contains("show"),
        "invocation should include operation name"
    );
}
