#![cfg(unix)]

use needle::bead_store::{
    builtin_bead_backends, load_bead_backends_with_sources, open_configured_in,
    resolve_configured_backend_in, BeadStore,
};
use needle::config::{BeadBackend as ConfigBackend, BeadCliConfig};
use needle::types::{BeadId, ClaimResult};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn descriptor(directory: &Path, binary: &Path, identity_pattern: &str) {
    let mut descriptor = builtin_bead_backends().remove(0);
    descriptor.name = "fixture-remote".to_string();
    descriptor.binary = "fixture-remote-cli".to_string();
    descriptor.detect_paths = vec![binary.to_path_buf()];
    descriptor.identity_pattern = identity_pattern.to_string();
    descriptor.verified_against = "fixture-remote 1.0.0".to_string();
    descriptor.verified_on = "2026-09-05".to_string();
    descriptor.operations.get_mut("claim").unwrap().argv = vec![
        "claim-one".to_string(),
        "--id".to_string(),
        "{id}".to_string(),
        "--actor".to_string(),
        "{actor}".to_string(),
    ];
    descriptor.operations.get_mut("claim").unwrap().strategy = Some("atomic_command".to_string());
    descriptor.operations.get_mut("show").unwrap().argv =
        vec!["get".to_string(), "--id".to_string(), "{id}".to_string()];
    descriptor.operations.get_mut("release").unwrap().argv = vec![
        "release-one".to_string(),
        "--id".to_string(),
        "{id}".to_string(),
    ];
    fs::write(
        directory.join("fixture-remote.yaml"),
        serde_yaml::to_string(&descriptor).unwrap(),
    )
    .unwrap();
}

fn config() -> BeadCliConfig {
    BeadCliConfig {
        backend: ConfigBackend::External("fixture-remote".to_string()),
        path: None,
    }
}

fn fixture_script(version: &str, claim_response: &str) -> String {
    format!(
        r#"#!/bin/sh
case "$1" in
  --version)
    printf '%s\n' '{version}'
    ;;
  claim-one)
    printf '<%s>\n' "$@" >> invocations.log
    printf '%s\n' '{claim_response}'
    ;;
  get)
    printf '<%s>\n' "$@" >> invocations.log
    printf '%s\n' '{{"id":"work:alpha/1","title":"fixture","description":null,"priority":2,"status":"in_progress","assignee":"worker with spaces","labels":[],"source_repo":"","dependencies":[],"dependents":[],"comments":[],"created_at":"2026-09-05T00:00:00Z","updated_at":"2026-09-05T00:00:01Z"}}'
    ;;
  release-one)
    printf '<%s>\n' "$@" >> invocations.log
    touch released
    ;;
esac
"#
    )
}

#[test]
fn resolves_external_descriptor_with_provenance() {
    let root = tempfile::tempdir().unwrap();
    let descriptors = root.path().join("descriptors");
    fs::create_dir(&descriptors).unwrap();
    let binary = root.path().join("fixture remote cli");
    executable(
        &binary,
        &fixture_script("fixture-remote 1.0.0", r#"{"outcome":"not_claimable"}"#),
    );
    descriptor(&descriptors, &binary, r"^fixture-remote 1\.0\.0");

    let resolved = resolve_configured_backend_in(&config(), &descriptors).unwrap();
    assert_eq!(resolved.descriptor.name, "fixture-remote");
    assert_eq!(resolved.binary, binary);
    assert_eq!(
        resolved.descriptor_source,
        descriptors.join("fixture-remote.yaml")
    );
}

#[tokio::test]
async fn external_atomic_claim_uses_descriptor_argv_without_shell_splitting() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let descriptors = root.path().join("descriptors");
    fs::create_dir(&workspace).unwrap();
    fs::create_dir(&descriptors).unwrap();
    let binary = root.path().join("fixture remote cli");
    executable(
        &binary,
        &fixture_script(
            "fixture-remote 1.0.0",
            r#"{"outcome":"claimed","bead_id":"work:alpha/1"}"#,
        ),
    );
    descriptor(&descriptors, &binary, r"^fixture-remote 1\.0\.0");

    let store: std::sync::Arc<dyn BeadStore> =
        open_configured_in(&config(), workspace.clone(), None, None, None, &descriptors).unwrap();
    let commands = store.prompt_commands().unwrap();
    assert_eq!(commands.cli, format!("'{}'", binary.display()));
    assert!(commands
        .dep_add
        .starts_with(&format!("'{}'", binary.display())));
    assert!(commands.dep_add.contains("<blocked-id> <blocker-id>"));
    let result = store
        .claim(&BeadId::from("work:alpha/1"), "worker with spaces")
        .await
        .unwrap();
    assert!(matches!(result, ClaimResult::Claimed(_)));
    store.release(&BeadId::from("work:alpha/1")).await.unwrap();

    let log = fs::read_to_string(workspace.join("invocations.log")).unwrap();
    assert!(log.contains("<claim-one>\n<--id>\n<work:alpha/1>\n<--actor>\n<worker with spaces>\n"));
    assert!(log.contains("<release-one>\n<--id>\n<work:alpha/1>\n"));
    assert!(workspace.join("released").exists());
}

#[test]
fn identity_mismatch_fails_before_store_mutation() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let descriptors = root.path().join("descriptors");
    fs::create_dir(&workspace).unwrap();
    fs::create_dir(&descriptors).unwrap();
    let binary = root.path().join("fixture-remote-cli");
    executable(
        &binary,
        &fixture_script("different-cli 9.9.9", r#"{"outcome":"claimed"}"#),
    );
    descriptor(&descriptors, &binary, r"^fixture-remote 1\.0\.0");

    let error = open_configured_in(&config(), workspace.clone(), None, None, None, &descriptors)
        .err()
        .expect("identity mismatch must fail")
        .to_string();
    assert!(error.contains("identity mismatch"), "{error}");
    assert!(!workspace.join("invocations.log").exists());
    assert!(!workspace.join("released").exists());
}

#[test]
fn unknown_binding_does_not_fall_back_to_native_backend() {
    let descriptors = tempfile::tempdir().unwrap();
    let missing = BeadCliConfig {
        backend: ConfigBackend::External("not-installed".to_string()),
        path: Some(PathBuf::from("/bin/true")),
    };
    let error = resolve_configured_backend_in(&missing, descriptors.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("not-installed"));
    assert!(error.contains("was not found"));
}

#[test]
fn duplicate_operator_descriptors_are_rejected_as_ambiguous() {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("fixture-remote-cli");
    executable(
        &binary,
        &fixture_script("fixture-remote 1.0.0", r#"{"outcome":"not_claimable"}"#),
    );
    descriptor(root.path(), &binary, r"^fixture-remote 1\.0\.0");
    fs::copy(
        root.path().join("fixture-remote.yaml"),
        root.path().join("fixture-remote-copy.yaml"),
    )
    .unwrap();

    let error = load_bead_backends_with_sources(root.path(), &builtin_bead_backends())
        .unwrap_err()
        .to_string();
    assert!(error.contains("ambiguous"), "{error}");
    assert!(error.contains("fixture-remote.yaml"), "{error}");
    assert!(error.contains("fixture-remote-copy.yaml"), "{error}");
}

#[tokio::test]
async fn malformed_atomic_claim_response_is_an_error_not_an_empty_queue() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let descriptors = root.path().join("descriptors");
    fs::create_dir(&workspace).unwrap();
    fs::create_dir(&descriptors).unwrap();
    let binary = root.path().join("fixture-remote-cli");
    executable(
        &binary,
        &fixture_script("fixture-remote 1.0.0", r#"{"unexpected":true}"#),
    );
    descriptor(&descriptors, &binary, r"^fixture-remote 1\.0\.0");

    let store = open_configured_in(&config(), workspace, None, None, None, &descriptors).unwrap();
    let error = store
        .claim(&BeadId::from("work:alpha/1"), "worker")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("omitted normalized outcome"), "{error}");
}

#[tokio::test]
async fn atomic_claim_race_preserves_the_winning_actor() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let descriptors = root.path().join("descriptors");
    fs::create_dir(&workspace).unwrap();
    fs::create_dir(&descriptors).unwrap();
    let binary = root.path().join("fixture-remote-cli");
    executable(
        &binary,
        &fixture_script(
            "fixture-remote 1.0.0",
            r#"{"outcome":"race_lost","claimed_by":"worker-b"}"#,
        ),
    );
    descriptor(&descriptors, &binary, r"^fixture-remote 1\.0\.0");

    let store = open_configured_in(&config(), workspace, None, None, None, &descriptors).unwrap();
    let result = store
        .claim(&BeadId::from("work:alpha/1"), "worker-a")
        .await
        .unwrap();
    assert!(matches!(
        result,
        ClaimResult::RaceLost { claimed_by } if claimed_by == "worker-b"
    ));
}

#[tokio::test]
async fn concurrent_atomic_claims_have_exactly_one_winner() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let descriptors = root.path().join("descriptors");
    fs::create_dir(&workspace).unwrap();
    fs::create_dir(&descriptors).unwrap();
    let binary = root.path().join("fixture-remote-cli");
    executable(
        &binary,
        r#"#!/bin/sh
case "$1" in
  --version)
    echo 'fixture-remote 1.0.0'
    ;;
  claim-one)
    if mkdir claim-lock 2>/dev/null; then
      printf '%s\n' '{"outcome":"claimed","bead_id":"work:alpha/1"}'
    else
      printf '%s\n' '{"outcome":"race_lost","claimed_by":"winner"}'
    fi
    ;;
  get)
    printf '%s\n' '{"id":"work:alpha/1","title":"fixture","description":null,"priority":2,"status":"in_progress","assignee":"winner","labels":[],"source_repo":"","dependencies":[],"dependents":[],"comments":[],"created_at":"2026-09-05T00:00:00Z","updated_at":"2026-09-05T00:00:01Z"}'
    ;;
esac
"#,
    );
    descriptor(&descriptors, &binary, r"^fixture-remote 1\.0\.0");
    let store = open_configured_in(&config(), workspace, None, None, None, &descriptors).unwrap();
    let id = BeadId::from("work:alpha/1");

    let (first, second) = tokio::join!(store.claim(&id, "worker-a"), store.claim(&id, "worker-b"));
    let outcomes = [first.unwrap(), second.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ClaimResult::Claimed(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ClaimResult::RaceLost { .. }))
            .count(),
        1
    );
}
