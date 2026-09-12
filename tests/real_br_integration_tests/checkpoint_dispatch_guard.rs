#![cfg(unix)]
use needle::bead_store::{builtin_bead_backends, BeadStore, CliBeadStore};
use serde_json::json;
use std::{collections::HashMap, fs, os::unix::fs::PermissionsExt, path::Path};

fn status(path: &Path, relationship: &str, ready: bool) {
    fs::write(
        path.join("status.json"),
        json!({"relationship":relationship,
        "ready_to_commit":ready,"not_ready_reasons":["fixture reason"]})
        .to_string(),
    )
    .unwrap();
}

fn fixture(relationship: &str, ready: bool) -> (tempfile::TempDir, CliBeadStore) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join(".beads")).unwrap();
    fs::write(dir.path().join(".beads/config.json"), "{}").unwrap();
    status(dir.path(), relationship, ready);
    fs::write(
        dir.path().join("aligned.json"),
        json!({"relationship":"aligned","ready_to_commit":true}).to_string(),
    )
    .unwrap();
    let binary = dir.path().join("fixture-bead");
    fs::write(
        &binary,
        r#"#!/bin/sh
printf '%s\n' "$*" >> operations.txt
case "$1 $2" in
  'sync status') cat status.json ;;
  'sync reconcile'|'sync flush-only')
    if test -f fail-sync; then echo 'checkpoint_integrity_failure: fixture' >&2; exit 5; fi
    cp aligned.json status.json ;;
  *) printf '%s\n' '[]' ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|b| b.name == "bead-rs")
        .unwrap();
    let store =
        CliBeadStore::new(backend, binary, dir.path().to_path_buf(), None, None, None).unwrap();
    (dir, store)
}

#[tokio::test]
async fn divergent_workspace_never_reaches_claim_release_or_repair() {
    let (dir, store) = fixture("covered-ahead-integrity-failure", false);
    for operation in ["ready", "claim_auto", "release", "doctor_repair", "import"] {
        let values = HashMap::from([
            ("actor", "worker".to_string()),
            ("id", "fixture-1".to_string()),
            ("input", "fixture.jsonl".to_string()),
            ("mode", "merge".to_string()),
        ]);
        assert!(store.run_operation(operation, &values).await.is_err());
        assert!(store.workspace_pause_reason().is_some());
    }
    let calls = fs::read_to_string(dir.path().join("operations.txt")).unwrap();
    assert!(calls
        .lines()
        .all(|line| line == "sync status --format json"));
    assert!(store.full_rebuild().await.is_err());
    assert!(dir.path().join(".beads/config.json").exists());
}

#[tokio::test]
async fn remote_advancement_reconciles_before_work_and_healthy_recovery_clears_pause() {
    let (dir, store) = fixture("remote-advanced", false);
    store.run_operation("ready", &HashMap::new()).await.unwrap();
    let calls = fs::read_to_string(dir.path().join("operations.txt")).unwrap();
    assert_eq!(
        calls.lines().take(3).collect::<Vec<_>>(),
        [
            "sync status --format json",
            "sync reconcile --actor needle-sync",
            "sync status --format json"
        ]
    );
    status(dir.path(), "covered-ahead-integrity-failure", false);
    assert!(store.run_operation("ready", &HashMap::new()).await.is_err());
    status(dir.path(), "aligned", true);
    store.run_operation("ready", &HashMap::new()).await.unwrap();
    assert!(store.workspace_pause_reason().is_none());
}

#[tokio::test]
async fn failed_flush_pauses_only_its_workspace_and_malformed_status_fails_closed() {
    let (dir, store) = fixture("behind", false);
    fs::write(dir.path().join("fail-sync"), "").unwrap();
    assert!(store.run_operation("ready", &HashMap::new()).await.is_err());
    assert!(store.workspace_pause_reason().is_some());
    let (_healthy_dir, healthy) = fixture("aligned", true);
    healthy
        .run_operation("ready", &HashMap::new())
        .await
        .unwrap();
    assert!(healthy.workspace_pause_reason().is_none());
    fs::write(dir.path().join("status.json"), "{}").unwrap();
    assert!(store.run_operation("ready", &HashMap::new()).await.is_err());
}

#[tokio::test]
async fn non_owner_host_cannot_claim_and_owner_change_is_rechecked() {
    let (dir, store) = fixture("aligned", true);
    fs::write(
        dir.path().join(".needle.yaml"),
        "queue:\n  owner_host: fixture-non-owner-host\n",
    )
    .unwrap();
    assert!(store.run_operation("ready", &HashMap::new()).await.is_err());
    assert!(!dir.path().join("operations.txt").exists());
    let config = serde_yaml::to_string(
        &json!({"queue":{"owner_host":gethostname::gethostname().to_str().unwrap()}}),
    )
    .unwrap();
    fs::write(dir.path().join(".needle.yaml"), config).unwrap();
    store.run_operation("ready", &HashMap::new()).await.unwrap();
    assert!(store.workspace_pause_reason().is_none());
    fs::write(
        dir.path().join(".needle.yaml"),
        "queue:\n  owner_host: []\n",
    )
    .unwrap();
    assert!(store.run_operation("ready", &HashMap::new()).await.is_err());
}
