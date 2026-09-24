//! Integration coverage for the ADR-005 release-channel upgrade path.
//!
//! GitHub transport is deliberately kept out of these tests: the downloaded
//! bytes enter the same `stage_and_promote` path through `--from-file`, which
//! makes the canary, promotion, rollback, and hot-reload assertions
//! deterministic and network-independent.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use needle::canary::CanaryRunner;
use needle::upgrade::{check_hot_reload, HotReloadCheck};

/// Create the smallest real canary workspace accepted by [`CanaryRunner`].
///
/// The fixture uses the workspace's bound native bead-rs binary, while the
/// candidate worker receives an isolated HOME and Explore root. This exercises
/// the same process boundary as a production release without touching the
/// operator's queue or adapter configuration.
fn setup_upgrade_canary(home: &Path, expected: &str) -> anyhow::Result<String> {
    let canary = home.join(".needle/canary");
    fs::create_dir_all(&canary)?;

    let bead_home = canary.join(".test-home");
    fs::create_dir_all(&bead_home)?;
    let init = Command::new(super::bead_path())
        .current_dir(&canary)
        .env("HOME", &bead_home)
        .args(["init", "--prefix", "upgrade", "--skip-foreign-workspace"])
        .output()?;
    anyhow::ensure!(
        init.status.success(),
        "canary bead init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let bead_path = serde_json::to_string(&super::bead_path())?;
    fs::write(
        canary.join(".needle.yaml"),
        format!("bead_cli:\n  backend: bead-rs\n  path: {bead_path}\n"),
    )?;

    let bead_id = super::create_bead(&canary, "release canary")?.to_string();
    fs::create_dir_all(canary.join("expected"))?;
    fs::write(
        canary.join("expected").join(format!("{bead_id}.yaml")),
        expected,
    )?;
    Ok(bead_id)
}

fn run_upgrade_from_file(home: &Path, candidate: &Path) -> anyhow::Result<Output> {
    let bead_binary = super::bead_path();
    let bead_dir = bead_binary
        .parent()
        .ok_or_else(|| anyhow::anyhow!("native bead-rs binary has no parent directory"))?;
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![bead_dir.to_path_buf()];
    path_entries.extend(std::env::split_paths(&inherited_path));
    let path = std::env::join_paths(path_entries)
        .map_err(|error| anyhow::anyhow!("failed to construct candidate PATH: {error}"))?;

    Ok(Command::new(current_candidate())
        .args([
            "upgrade",
            "--from-file",
            candidate
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("candidate path is not valid UTF-8"))?,
        ])
        .env("HOME", home)
        .env("NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT", home)
        .env("PATH", path)
        // Release canaries own their timing and are not host-capacity tests.
        .env_remove("NEEDLE_START_DELAY")
        .env("NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK", "1")
        .output()?)
}

/// The `needle` binary under test. Nextest archive runs relocate it, so the
/// compile-time `CARGO_BIN_EXE_needle` path does not exist there.
fn current_candidate() -> PathBuf {
    std::env::var_os("NEXTEST_BIN_EXE_needle")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_needle")))
}

/// Keep the fixture outside any enclosing bead workspace. Bead-rs discovers a
/// workspace by walking upward, so another worker's `/tmp/.beads` store would
/// otherwise capture this fixture before its own `bead init` can create a
/// local store.
fn upgrade_fixture() -> tempfile::TempDir {
    let temp_root = [std::env::temp_dir(), PathBuf::from("/var/tmp")]
        .into_iter()
        .find(|candidate| {
            candidate.is_dir()
                && !candidate
                    .ancestors()
                    .any(|ancestor| ancestor.join(".beads").is_dir())
        })
        .expect("no temporary root outside an enclosing bead workspace");
    tempfile::Builder::new()
        .prefix("release-upgrade-")
        .tempdir_in(temp_root)
        .expect("failed to create isolated upgrade fixture")
}

fn create_stable(home: &Path, content: &[u8]) -> anyhow::Result<()> {
    let bin_dir = home.join(".needle/bin");
    fs::create_dir_all(&bin_dir)?;
    fs::write(bin_dir.join("needle-stable"), content)?;
    Ok(())
}

#[test]
fn upgrade_requires_a_canary_workspace() {
    let temp_dir = upgrade_fixture();
    let home = temp_dir.path();
    create_stable(home, b"known-good-stable").expect("failed to create stable binary");

    let output = run_upgrade_from_file(home, &current_candidate()).expect("upgrade failed to run");
    assert!(
        !output.status.success(),
        "upgrade without a canary unexpectedly succeeded: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(home.join(".needle/bin/needle-stable")).unwrap(),
        b"known-good-stable",
        "an unvalidated candidate must not replace stable"
    );
    assert!(
        !home.join(".needle/bin/needle-testing").exists(),
        "a candidate rejected for missing canary must not remain staged"
    );
}

#[test]
fn upgrade_rejects_failed_canary_without_touching_stable_or_rollback() {
    let temp_dir = upgrade_fixture();
    let home = temp_dir.path();
    create_stable(home, b"stable-before-rejected-upgrade").expect("failed to create stable binary");
    fs::write(
        home.join(".needle/bin/needle-stable.prev"),
        b"rollback-before-rejected-upgrade",
    )
    .expect("failed to create rollback binary");
    setup_upgrade_canary(home, "type: success\nfinal_status: closed\n")
        .expect("failed to create canary fixture");

    let candidate = temp_dir.path().join("bad-needle");
    fs::write(&candidate, b"#!/bin/sh\nexit 42\n").expect("failed to create bad candidate");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755))
            .expect("failed to make bad candidate executable");
    }

    let output = run_upgrade_from_file(home, &candidate).expect("upgrade failed to run");
    assert!(
        !output.status.success(),
        "failed canary upgrade unexpectedly succeeded: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(home.join(".needle/bin/needle-stable")).unwrap(),
        b"stable-before-rejected-upgrade"
    );
    assert_eq!(
        fs::read(home.join(".needle/bin/needle-stable.prev")).unwrap(),
        b"rollback-before-rejected-upgrade"
    );
    assert!(!home.join(".needle/bin/needle-testing").exists());
}

#[test]
fn first_promotion_creates_stable_without_a_rollback_file() {
    let temp_dir = upgrade_fixture();
    let home = temp_dir.path();
    setup_upgrade_canary(home, "type: success\nfinal_status: closed\n")
        .expect("failed to create canary fixture");

    let candidate = current_candidate();
    let output = run_upgrade_from_file(home, &candidate).expect("upgrade failed to run");
    assert!(
        output.status.success(),
        "first promotion failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(home.join(".needle/bin/needle-stable")).unwrap(),
        fs::read(&candidate).unwrap()
    );
    assert!(!home.join(".needle/bin/needle-stable.prev").exists());
    assert!(!home.join(".needle/bin/needle-testing").exists());
    assert!(home.join(".needle/bin/needle").exists());
}

#[test]
fn passing_promotion_exposes_stable_to_hot_reload() {
    let temp_dir = upgrade_fixture();
    let home = temp_dir.path();
    create_stable(home, b"stable-before-upgrade").expect("failed to create stable binary");
    setup_upgrade_canary(home, "type: success\nfinal_status: closed\n")
        .expect("failed to create canary fixture");

    let candidate = current_candidate();
    let output = run_upgrade_from_file(home, &candidate).expect("upgrade failed to run");
    assert!(
        output.status.success(),
        "passing canary upgrade failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(home.join(".needle/bin/needle-stable.prev")).unwrap(),
        b"stable-before-upgrade"
    );

    match check_hot_reload(&home.join(".needle")).expect("hot-reload check failed") {
        HotReloadCheck::NewBinaryDetected { stable_path, .. } => {
            assert_eq!(stable_path, home.join(".needle/bin/needle-stable"));
        }
        other => panic!("expected hot-reload detection after promotion, got {other:?}"),
    }
}

#[test]
fn rollback_restores_previous_stable_and_updates_symlink() {
    let temp_dir = upgrade_fixture();
    let home = temp_dir.path().to_path_buf();
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create release channel");
    fs::write(bin_dir.join("needle-testing"), b"candidate-v2").unwrap();
    fs::write(bin_dir.join("needle-stable"), b"stable-v1").unwrap();

    let runner = CanaryRunner::new(home.clone(), home.join("canary"), 30);
    runner.promote().expect("promotion failed");
    assert_eq!(fs::read(runner.prev_binary()).unwrap(), b"stable-v1");

    runner.rollback().expect("rollback failed");
    assert_eq!(fs::read(runner.stable_binary()).unwrap(), b"stable-v1");
    assert_eq!(
        fs::read(home.join("bin/needle-stable.rollback")).unwrap(),
        b"candidate-v2"
    );
    assert_eq!(
        fs::read_link(runner.symlink_path()).unwrap(),
        runner.stable_binary()
    );
}
