//! End-to-end coverage for the Unix hot-reload process handoff.
//!
//! Production systemd units launch a static path that may lag behind
//! `needle-stable`. A hot reload must therefore replace the worker process in
//! place; exiting and relying on systemd would relaunch the stale path.

#![cfg(unix)]

use needle::upgrade::{check_hot_reload, file_hash, re_exec_stable, HotReloadCheck};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

const HELPER_ENV: &str = "NEEDLE_HOT_RELOAD_EXEC_HELPER";
const STABLE_ENV: &str = "NEEDLE_HOT_RELOAD_STABLE";
const CAPTURE_ENV: &str = "NEEDLE_HOT_RELOAD_CAPTURE";

/// Subprocess-only helper. The parent filters to this exact test and sets
/// HELPER_ENV; `re_exec_stable` then replaces this test process with the stub
/// stable binary.
#[test]
fn re_exec_helper() {
    if std::env::var_os(HELPER_ENV).is_none() {
        return;
    }

    let stable = PathBuf::from(std::env::var_os(STABLE_ENV).expect("stable helper path"));
    let workspace = PathBuf::from("/tmp/needle hot reload workspace");
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let result = runtime.block_on(re_exec_stable(
        &stable,
        "hot-reload-worker",
        Some(&workspace),
        Some("test-agent"),
        Some(321),
    ));

    panic!("successful re-exec must not return; result: {result:?}");
}

#[test]
fn hot_reload_exec_preserves_pid_and_resume_arguments() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let stable = temp.path().join("needle-stable");
    let capture = temp.path().join("capture.txt");

    fs::write(
        &stable,
        "#!/bin/sh\nprintf '%s\\n' \"$$\" \"$@\" > \"$NEEDLE_HOT_RELOAD_CAPTURE\"\n",
    )
    .expect("write stable stub");
    let mut permissions = fs::metadata(&stable).expect("stub metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stable, permissions).expect("make stable stub executable");

    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg("re_exec_helper")
        .arg("--nocapture")
        .env(HELPER_ENV, "1")
        .env(STABLE_ENV, &stable)
        .env(CAPTURE_ENV, &capture)
        .spawn()
        .expect("spawn re-exec helper");
    let original_pid = child.id();
    let status = child.wait().expect("wait for re-exec helper");
    assert!(status.success(), "replacement binary exited with {status}");

    let lines: Vec<String> = fs::read_to_string(&capture)
        .expect("read replacement capture")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(lines[0], original_pid.to_string());
    assert_eq!(
        &lines[1..],
        [
            "run",
            "--resume",
            "--identifier",
            "hot-reload-worker",
            "--count",
            "1",
            "--workspace",
            "/tmp/needle hot reload workspace",
            "--agent",
            "test-agent",
            "--timeout",
            "321",
        ]
    );
}

#[test]
fn hot_reload_detects_a_later_rollback_hash() {
    let home = tempfile::tempdir().expect("temporary NEEDLE home");
    let bin_dir = home.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("create bin directory");
    let stable = bin_dir.join("needle-stable");

    fs::write(&stable, b"release candidate").expect("write candidate");
    let candidate_hash = file_hash(&stable).expect("candidate hash");
    let first = check_hot_reload(home.path()).expect("first hot-reload check");
    assert!(matches!(
        first,
        HotReloadCheck::NewBinaryDetected { new_hash, .. } if new_hash == candidate_hash
    ));

    fs::write(&stable, b"stable.prev rollback").expect("write rollback");
    let rollback_hash = file_hash(&stable).expect("rollback hash");
    assert_ne!(candidate_hash, rollback_hash);
    let second = check_hot_reload(home.path()).expect("rollback hot-reload check");
    assert!(matches!(
        second,
        HotReloadCheck::NewBinaryDetected { new_hash, .. } if new_hash == rollback_hash
    ));
}

#[tokio::test]
async fn re_exec_failure_returns_without_replacing_process() {
    let missing = PathBuf::from("/definitely/missing/needle-stable");
    let error = re_exec_stable(&missing, "worker", None, None, None)
        .await
        .expect_err("missing stable binary must fail");
    assert!(error.to_string().contains("failed to exec :stable binary"));
}
