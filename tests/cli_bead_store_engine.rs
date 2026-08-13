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
fn store(root: &Path, backend_name: &str) -> CliBeadStore {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == backend_name)
        .unwrap();
    let binary = root.join("fixture-cli");
    executable(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> invocations.log\nprintf '%s\\n' '[{\"id\":\"fixture-1\",\"title\":\"fixture\",\"description\":null,\"notes\":\"backend note\",\"priority\":2,\"status\":\"open\",\"assignee\":null,\"labels\":[],\"source_repo\":\"\",\"dependencies\":[],\"dependents\":[],\"comments\":[],\"created_at\":\"2026-08-12T00:00:00Z\",\"updated_at\":\"2026-08-12T00:00:00Z\"}]'\n",
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

#[tokio::test]
#[cfg(unix)]
async fn notes_use_the_bound_backend_show_operation() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path(), "bead-rs");
    assert_eq!(
        store.notes(&BeadId::from("fixture-1")).await.unwrap(),
        Some("backend note".to_string())
    );
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

#[test]
#[cfg(unix)]
fn bead_forge_list_shape_matches_installed_json_lines_contract() {
    let root = tempfile::tempdir().unwrap();
    let record = |id: &str| {
        format!(
            r#"{{"id":"{id}","title":"fixture","description":"","priority":2,"status":"open","assignee":null,"labels":[],"source_repo":".","dependencies":[],"created_at":"2026-08-12T00:00:00Z","updated_at":"2026-08-12T00:00:00Z"}}"#
        )
    };
    let output = format!("{}\n{}\n", record("bf-a"), record("bf-b"));
    let beads = store(root.path(), "bead-forge")
        .parse_beads("list_all", &output)
        .unwrap();
    assert_eq!(beads.len(), 2);
    assert_eq!(beads[0].id, BeadId::from("bf-a"));
    assert_eq!(beads[1].id, BeadId::from("bf-b"));
}

#[tokio::test]
#[cfg(unix)]
async fn explicit_bead_rs_claim_uses_revision_guard() {
    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let binary = root.path().join("fixture-cli");
    executable(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> invocations.log\nif [ \"$1\" = show ]; then\n  printf '%s\\n' '[{\"id\":\"fixture-1\",\"title\":\"fixture\",\"description\":null,\"priority\":2,\"status\":\"open\",\"assignee\":null,\"labels\":[],\"source_repo\":\"\",\"dependencies\":[],\"dependents\":[],\"comments\":[],\"created_at\":\"2026-08-12T00:00:00Z\",\"updated_at\":\"2026-08-12T00:00:00Z\",\"revision\":7}]'\nfi\n",
    );
    let store =
        CliBeadStore::new(backend, binary, root.path().to_path_buf(), None, None, None).unwrap();

    let result = store
        .claim(&BeadId::from("fixture-1"), "worker-a")
        .await
        .unwrap();
    assert!(matches!(result, ClaimResult::Claimed(_)));
    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert!(invocations.contains(
        "update\nfixture-1\n--status\nin_progress\n--assignee\nworker-a\n--if-revision\n7\n"
    ));
}

#[tokio::test]
#[cfg(unix)]
async fn bead_forge_release_uses_atomic_batch_update() {
    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-forge")
        .unwrap();
    let binary = root.path().join("fixture-cli");
    executable(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> invocations.log\n",
    );
    let store =
        CliBeadStore::new(backend, binary, root.path().to_path_buf(), None, None, None).unwrap();

    store.release(&BeadId::from("bf-1")).await.unwrap();
    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert!(invocations.starts_with("batch\n--json\n"));
    assert!(invocations.contains(r#"[{"assignee":"","id":"bf-1","op":"update","status":"open"}]"#));
}

#[tokio::test]
#[cfg(unix)]
async fn bead_forge_explicit_claim_uses_atomic_batch_update() {
    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-forge")
        .unwrap();
    let binary = root.path().join("fixture-cli");
    executable(
        &binary,
        r#"#!/bin/sh
printf '%s\n' "$@" >> invocations.log
if [ "$1" = show ]; then
  if [ -f claimed ]; then
    printf '%s\n' '[{"id":"bf-1","title":"fixture","description":null,"priority":2,"status":"in_progress","assignee":"worker-a","labels":[],"source_repo":"","dependencies":[],"dependents":[],"comments":[],"created_at":"2026-08-12T00:00:00Z","updated_at":"2026-08-12T00:00:00Z"}]'
  else
    printf '%s\n' '[{"id":"bf-1","title":"fixture","description":null,"priority":2,"status":"open","assignee":null,"labels":[],"source_repo":"","dependencies":[],"dependents":[],"comments":[],"created_at":"2026-08-12T00:00:00Z","updated_at":"2026-08-12T00:00:00Z"}]'
  fi
elif [ "$1" = batch ]; then
  touch claimed
fi
"#,
    );
    let store =
        CliBeadStore::new(backend, binary, root.path().to_path_buf(), None, None, None).unwrap();

    let result = store
        .claim(&BeadId::from("bf-1"), "worker-a")
        .await
        .unwrap();
    assert!(matches!(result, ClaimResult::Claimed(_)));
    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert!(invocations.contains("batch\n--json\n"));
    assert!(invocations
        .contains(r#"[{"assignee":"worker-a","id":"bf-1","op":"update","status":"in_progress"}]"#));
    assert!(!invocations.contains("update\nbf-1\n--assignee\n"));
}

#[tokio::test]
#[cfg(unix)]
async fn bead_forge_clear_assignee_uses_atomic_batch_update() {
    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-forge")
        .unwrap();
    let binary = root.path().join("fixture-cli");
    executable(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> invocations.log\n",
    );
    let store =
        CliBeadStore::new(backend, binary, root.path().to_path_buf(), None, None, None).unwrap();

    store.clear_assignee(&BeadId::from("bf-1")).await.unwrap();
    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert!(invocations.starts_with("batch\n--json\n"));
    assert!(invocations.contains(r#"[{"assignee":"","id":"bf-1","op":"update"}]"#));
    assert!(!invocations.contains("update\nbf-1\n--assignee\n"));
}

#[tokio::test]
#[cfg(unix)]
async fn bead_forge_split_is_one_transactional_batch() {
    use needle::bead_store::NewChild;

    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-forge")
        .unwrap();
    let binary = root.path().join("fixture-cli");
    executable(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> invocations.log\nprintf '%s\\n' '[op 0] ok: bf-child-a' '[op 1] ok: bf-child-b' '[op 2] ok' '[op 3] ok'\n",
    );
    let store =
        CliBeadStore::new(backend, binary, root.path().to_path_buf(), None, None, None).unwrap();
    let labels_a = ["one"];
    let labels_b = ["two"];
    let children = [
        NewChild {
            title: "A",
            body: "body A",
            labels: &labels_a,
        },
        NewChild {
            title: "B",
            body: "body B",
            labels: &labels_b,
        },
    ];

    let ids = store
        .split_bead(&BeadId::from("bf-parent"), &children)
        .await
        .unwrap();
    assert_eq!(
        ids,
        [BeadId::from("bf-child-a"), BeadId::from("bf-child-b")]
    );
    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert_eq!(invocations.matches("batch\n").count(), 1);
    let payload: serde_json::Value =
        serde_json::from_str(invocations.lines().nth(2).unwrap()).unwrap();
    assert_eq!(payload[2]["op"], "dep_add_blocker");
    assert_eq!(payload[2]["id"], "bf-parent");
    assert_eq!(payload[2]["blocker"], "@0");
    assert_eq!(payload[3]["op"], "dep_add_blocker");
    assert_eq!(payload[3]["id"], "bf-parent");
    assert_eq!(payload[3]["blocker"], "@1");
}
