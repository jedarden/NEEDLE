use needle::bead_store::{builtin_bead_backends, BeadStore, CliBeadStore};
use needle::process_runner::{ProcessRequest, ProcessRunner, TokioProcessRunner};
use needle::types::{BeadId, ClaimResult};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

#[cfg(unix)]
fn batch_backend() -> needle::bead_store::BeadBackend {
    let mut backend = builtin_bead_backends().into_iter().next().unwrap();
    backend.name = "bead-forge-fixture".to_string();
    backend.operations.get_mut("claim").unwrap().strategy = Some("batch_op".to_string());
    backend
}

#[cfg(unix)]
#[derive(Clone)]
struct TraceCapture(Arc<Mutex<Vec<u8>>>);

#[cfg(unix)]
struct TraceWriter(Arc<Mutex<Vec<u8>>>);

#[cfg(unix)]
impl Write for TraceWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TraceCapture {
    type Writer = TraceWriter;

    fn make_writer(&'a self) -> Self::Writer {
        TraceWriter(Arc::clone(&self.0))
    }
}

#[cfg(unix)]
fn trace_capture() -> (TraceCapture, tracing::subscriber::DefaultGuard) {
    let capture = TraceCapture(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    (capture, guard)
}

#[cfg(unix)]
fn captured_trace(capture: &TraceCapture) -> String {
    String::from_utf8(capture.0.lock().unwrap().clone()).unwrap()
}

#[cfg(unix)]
fn etxtbsy_fixture(directory: &Path) -> std::path::PathBuf {
    let binary = directory.join("etxtbsy-fixture");
    executable(&binary, "#!/bin/sh\nprintf 'ready\\n'\n");
    binary
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
fn rendering_uses_bead_rs_dependency_orientation() {
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
}

#[test]
#[cfg(unix)]
fn absent_optional_velocity_values_remove_their_flags() {
    let root = tempfile::tempdir().unwrap();
    let values = HashMap::from([("actor", "worker".to_string())]);
    assert_eq!(
        store(root.path(), "bead-rs")
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
fn bead_rs_list_shape_matches_installed_json_lines_contract() {
    let root = tempfile::tempdir().unwrap();
    let record = |id: &str| {
        format!(
            r#"{{"id":"{id}","title":"fixture","description":"","priority":2,"status":"open","assignee":null,"labels":[],"source_repo":".","dependencies":[],"created_at":"2026-08-12T00:00:00Z","updated_at":"2026-08-12T00:00:00Z"}}"#
        )
    };
    let output = format!("{}\n{}\n", record("bead-a"), record("bead-b"));
    let beads = store(root.path(), "bead-rs")
        .parse_beads("list_all", &output)
        .unwrap();
    assert_eq!(beads.len(), 2);
    assert_eq!(beads[0].id, BeadId::from("bead-a"));
    assert_eq!(beads[1].id, BeadId::from("bead-b"));
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
async fn bead_rs_release_uses_descriptor_command() {
    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let binary = root.path().join("fixture-cli");
    executable(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> invocations.log\n",
    );
    let store =
        CliBeadStore::new(backend, binary, root.path().to_path_buf(), None, None, None).unwrap();

    store.release(&BeadId::from("bead-1")).await.unwrap();
    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert_eq!(invocations, "release\nbead-1\n");
}

#[tokio::test]
#[cfg(unix)]
async fn bead_rs_explicit_claim_uses_revision_guarded_update() {
    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let binary = root.path().join("fixture-cli");
    executable(
        &binary,
        r#"#!/bin/sh
printf '%s\n' "$@" >> invocations.log
if [ "$1" = show ]; then
  if [ -f claimed ]; then
    printf '%s\n' '[{"id":"bead-1","title":"fixture","description":null,"priority":2,"status":"in_progress","assignee":"worker-a","labels":[],"source_repo":"","dependencies":[],"dependents":[],"comments":[],"created_at":"2026-08-12T00:00:00Z","updated_at":"2026-08-12T00:00:00Z","revision":8}]'
  else
    printf '%s\n' '[{"id":"bead-1","title":"fixture","description":null,"priority":2,"status":"open","assignee":null,"labels":[],"source_repo":"","dependencies":[],"dependents":[],"comments":[],"created_at":"2026-08-12T00:00:00Z","updated_at":"2026-08-12T00:00:00Z","revision":7}]'
  fi
elif [ "$1" = update ]; then
  touch claimed
fi
"#,
    );
    let store =
        CliBeadStore::new(backend, binary, root.path().to_path_buf(), None, None, None).unwrap();

    let result = store
        .claim(&BeadId::from("bead-1"), "worker-a")
        .await
        .unwrap();
    assert!(matches!(result, ClaimResult::Claimed(_)));
    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert_eq!(invocations.matches("show\n").count(), 4);
    assert!(invocations.contains(
        "update\nbead-1\n--status\nin_progress\n--assignee\nworker-a\n--if-revision\n7\n"
    ));
    assert!(!invocations.contains("batch\n"));
}

#[tokio::test]
#[cfg(unix)]
async fn bead_rs_clear_assignee_uses_descriptor_command() {
    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let binary = root.path().join("fixture-cli");
    executable(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> invocations.log\n",
    );
    let store =
        CliBeadStore::new(backend, binary, root.path().to_path_buf(), None, None, None).unwrap();

    store.clear_assignee(&BeadId::from("bead-1")).await.unwrap();
    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert_eq!(invocations, "update\nbead-1\n--clear-assignee\n");
}

#[tokio::test]
#[cfg(unix)]
async fn bead_rs_split_uses_one_manifest_transaction() {
    use needle::bead_store::NewChild;

    let root = tempfile::tempdir().unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let binary = root.path().join("fixture-cli");
    executable(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> invocations.log\nif [ \"$1\" = manifest ]; then\n  grep -q '\"resource_keys\":\\[\"resource-a\"\\]' \"$4\" || exit 41\n  grep -q '\"blocked\":\"bead-parent\"' \"$4\" || exit 42\n  grep -q '\"blocker\":\"$needle_child_0\"' \"$4\" || exit 43\n  printf '%s\\n' '{\"manifest_version\":1,\"committed\":true,\"results\":[{\"op\":\"create\",\"issue_id\":\"bead-child-a\"},{\"op\":\"create\",\"issue_id\":\"bead-child-b\"}]}'\nfi\n",
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
            resource_keys: &["resource-a"],
        },
        NewChild {
            title: "B",
            body: "body B",
            labels: &labels_b,
            resource_keys: &[],
        },
    ];

    let ids = store
        .split_bead(&BeadId::from("bead-parent"), &children)
        .await
        .unwrap();
    assert_eq!(
        ids,
        [BeadId::from("bead-child-a"), BeadId::from("bead-child-b")]
    );
    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    let invocation: Vec<&str> = invocations.lines().collect();
    assert_eq!(invocation.first(), Some(&"manifest"));
    assert_eq!(invocation.get(1), Some(&"commit"));
    assert_eq!(invocation.get(2), Some(&"--input"));
    assert!(invocation.contains(&"--format"));
    assert!(!invocations.contains("create\n"));
    assert!(!invocations.contains("dep\n"));
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn bead_forge_batch_claim_completes_without_a_scheduler_race() {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("fixture-cli");
    executable(
        &binary,
        r##"#!/bin/sh
printf '%s\n' "$@" >> invocations.log
if [ "$1" = show ]; then
  if [ -e batch-seen ]; then
    printf '%s\n' '[{"id":"bead-1","title":"fixture","description":null,"priority":2,"status":"in_progress","assignee":"worker-a","labels":[],"source_repo":"","dependencies":[],"dependents":[],"comments":[],"created_at":"2026-08-12T00:00:00Z","updated_at":"2026-08-12T00:00:00Z"}]'
  else
    printf '%s\n' '[{"id":"bead-1","title":"fixture","description":null,"priority":2,"status":"open","assignee":null,"labels":[],"source_repo":"","dependencies":[],"dependents":[],"comments":[],"created_at":"2026-08-12T00:00:00Z","updated_at":"2026-08-12T00:00:00Z"}]'
  fi
elif [ "$1" = batch ]; then
  touch batch-seen
fi
"##,
    );
    let store = CliBeadStore::new(
        batch_backend(),
        binary,
        root.path().to_path_buf(),
        None,
        None,
        None,
    )
    .unwrap();

    let result = store.claim(&BeadId::from("bead-1"), "worker-a").await;

    let claimed = result.unwrap();
    assert!(matches!(claimed, ClaimResult::Claimed(_)));

    let invocations = fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert_eq!(invocations.matches("show\n").count(), 2);
    assert_eq!(invocations.matches("batch\n").count(), 1);
    assert!(invocations.contains(
        r#"[{"assignee":"worker-a","id":"bead-1","op":"update","status":"in_progress"}]"#
    ));
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn captured_process_retry_is_deterministic_bounded_and_observable() {
    let root = tempfile::tempdir().unwrap();
    let binary = etxtbsy_fixture(root.path());
    let write_guard = OpenOptions::new().write(true).open(&binary).unwrap();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1)).await;
        drop(write_guard);
    });
    let (capture, subscriber_guard) = trace_capture();

    let output = TokioProcessRunner
        .output(ProcessRequest::new(&binary), Duration::from_secs(1))
        .await
        .unwrap();
    release.await.unwrap();
    drop(subscriber_guard);

    assert!(output.success);
    assert_eq!(output.stdout, b"ready\n");
    let logs = captured_trace(&capture);
    assert_eq!(logs.matches("Retrying captured process spawn").count(), 1);
    assert!(logs.contains("Captured process spawn succeeded after ETXTBSY retry"));

    let persistent_guard = OpenOptions::new().write(true).open(&binary).unwrap();
    let (capture, subscriber_guard) = trace_capture();
    let error = TokioProcessRunner
        .output(ProcessRequest::new(&binary), Duration::from_secs(1))
        .await
        .unwrap_err();
    drop(subscriber_guard);
    drop(persistent_guard);

    assert!(error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| io_error.raw_os_error() == Some(26))
    }));
    assert_eq!(
        captured_trace(&capture)
            .matches("Retrying captured process spawn")
            .count(),
        4
    );

    let missing = root.path().join("missing-cli");
    let (capture, subscriber_guard) = trace_capture();
    let error = TokioProcessRunner
        .output(ProcessRequest::new(&missing), Duration::from_secs(1))
        .await
        .unwrap_err();
    drop(subscriber_guard);

    assert!(error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::NotFound)
    }));
    assert!(!captured_trace(&capture).contains("Retrying captured process spawn"));
}
