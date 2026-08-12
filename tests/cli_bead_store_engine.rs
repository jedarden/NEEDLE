use needle::bead_store::{builtin_bead_backends, CliBeadStore};
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
fn store(root: &Path, backend_name: &str) -> CliBeadStore {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == backend_name)
        .unwrap();
    let binary = root.join("fixture-cli");
    executable(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> invocations.log\nprintf '%s\\n' '[{\"id\":\"fixture-1\",\"title\":\"fixture\",\"description\":null,\"priority\":2,\"status\":\"open\",\"assignee\":null,\"labels\":[],\"source_repo\":\"\",\"dependencies\":[],\"dependents\":[],\"comments\":[],\"created_at\":\"2026-08-12T00:00:00Z\",\"updated_at\":\"2026-08-12T00:00:00Z\"}]'\n",
    );
    CliBeadStore::new(backend, binary, root.to_path_buf(), None, None, None).unwrap()
}

#[test]
#[cfg(unix)]
fn descriptor_and_binary_are_bound_together() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path(), "bead-rs");
    assert_eq!(store.backend().name, "bead-rs");
    assert_eq!(store.binary(), root.path().join("fixture-cli"));
    assert_eq!(store.workspace(), root.path());
}

#[test]
#[cfg(unix)]
fn rendering_preserves_dialect_specific_dependency_orientation() {
    let root = tempfile::tempdir().unwrap();
    let values = HashMap::from([
        ("blocked", "blocked-1".to_string()),
        ("blocker", "blocker-1".to_string()),
    ]);

    assert_eq!(
        store(root.path(), "bead-rs")
            .render_operation("dep_add", &values)
            .unwrap(),
        ["dep", "add", "blocked-1", "blocker-1", "--kind", "blocks"]
    );
    assert_eq!(
        store(root.path(), "bead-forge")
            .render_operation("dep_add", &values)
            .unwrap(),
        ["dep", "add", "blocker-1", "--blocks", "blocked-1"]
    );
}

#[test]
#[cfg(unix)]
fn absent_optional_velocity_values_remove_their_flags() {
    let root = tempfile::tempdir().unwrap();
    let values = HashMap::from([("actor", "worker".to_string())]);
    assert_eq!(
        store(root.path(), "bead-forge")
            .render_operation("claim_auto", &values)
            .unwrap(),
        ["claim", "--assignee", "worker", "--json"]
    );
}

#[tokio::test]
#[cfg(unix)]
async fn operation_execution_uses_bound_binary_and_workspace() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path(), "bead-rs");
    let output = store
        .run_operation("show", &HashMap::from([("id", "fixture-1".to_string())]))
        .await
        .unwrap();
    let beads = store.parse_beads("show", &output).unwrap();

    assert_eq!(beads.len(), 1);
    assert_eq!(beads[0].id.as_ref(), "fixture-1");
    assert_eq!(
        fs::read_to_string(root.path().join("invocations.log")).unwrap(),
        "show\nfixture-1\n--json\n"
    );
}

#[test]
#[cfg(unix)]
fn missing_required_runtime_value_fails_before_process_execution() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path(), "bead-rs");
    let error = store
        .render_operation("show", &HashMap::new())
        .unwrap_err()
        .to_string();
    assert!(error.contains("requires placeholder '{id}'"));
    assert!(!root.path().join("invocations.log").exists());
}
