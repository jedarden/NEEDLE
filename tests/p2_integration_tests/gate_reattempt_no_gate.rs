use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use needle::config::DefaultGatesConfig;
use needle::gate_health::{self, GateHealthState};
use needle::gate_reattempt::{self, GateReattemptConfig, IdleProbeOutcome, ProbeVerdict};
use tempfile::TempDir;

fn run_git(workspace: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .expect("git starts");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

struct EnvGuard {
    home: Option<std::ffi::OsString>,
    state_dir: Option<std::ffi::OsString>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.home.take() {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match self.state_dir.take() {
            Some(value) => std::env::set_var("NEEDLE_STATE_DIR", value),
            None => std::env::remove_var("NEEDLE_STATE_DIR"),
        }
    }
}

fn isolated_workspace() -> (EnvGuard, TempDir, TempDir) {
    let fixture = TempDir::new().unwrap();
    let home = TempDir::new_in(fixture.path()).unwrap();
    let guard = EnvGuard {
        home: std::env::var_os("HOME"),
        state_dir: std::env::var_os("NEEDLE_STATE_DIR"),
    };
    std::env::set_var("HOME", home.path());
    std::env::set_var("NEEDLE_STATE_DIR", home.path().join(".needle"));
    let workspace = TempDir::new_in(fixture.path()).unwrap();
    std::fs::write(
        workspace.path().join(".needle.yaml"),
        "validation:\n  fallback_gate: false\n",
    )
    .unwrap();
    (guard, fixture, workspace)
}

fn degraded(workspace: &Path, gate: &str) {
    gate_health::save_state(&GateHealthState {
        workspace: std::fs::canonicalize(workspace).unwrap(),
        consecutive_errors: 3,
        last_error_at: Utc::now().to_rfc3339(),
        last_command: "probe fixture".to_string(),
        last_reason: "fixture degradation".to_string(),
        degraded: true,
        fingerprint_window: Vec::new(),
        degraded_fingerprint: Some("fixture-fingerprint".to_string()),
        degraded_gate: Some(gate.to_string()),
        degraded_summary: Some("fixture".to_string()),
    })
    .unwrap();
}

fn config() -> GateReattemptConfig {
    GateReattemptConfig {
        enabled: true,
        initial_backoff_secs: 1,
        max_backoff_secs: 2,
        ..Default::default()
    }
}

#[tokio::test]
#[serial_test::serial(nt_process_env)]
async fn close_verification_without_workspace_gates_restores() -> Result<()> {
    let (_guard, _fixture, workspace) = isolated_workspace();
    degraded(workspace.path(), "close_verification");
    let restored = Arc::new(AtomicUsize::new(0));
    let restored_by_probe = Arc::clone(&restored);

    let outcome = gate_reattempt::probe_most_overdue(
        &config(),
        &DefaultGatesConfig::default(),
        Duration::from_secs(1),
        move |workspace, _gates_run| async move {
            restored_by_probe.fetch_add(1, Ordering::Relaxed);
            gate_health::clear_state(&workspace)?;
            Ok(())
        },
    )
    .await?;

    assert_eq!(restored.load(Ordering::Relaxed), 1);
    assert!(matches!(
        outcome,
        IdleProbeOutcome::Probed {
            verdict: ProbeVerdict::Passed { gates_run: 0 },
            ..
        }
    ));
    assert!(!gate_health::is_degraded(workspace.path())?);
    Ok(())
}

#[tokio::test]
#[serial_test::serial(nt_process_env)]
async fn clean_tree_degradation_restores_clean_and_stays_degraded_dirty() -> Result<()> {
    let (_guard, _fixture, workspace) = isolated_workspace();
    run_git(workspace.path(), &["init", "--quiet"]);
    run_git(workspace.path(), &["add", ".needle.yaml"]);
    run_git(
        workspace.path(),
        &[
            "-c",
            "user.name=NEEDLE test",
            "-c",
            "user.email=needle-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );

    degraded(workspace.path(), "fallback_clean_tree");
    let restored = Arc::new(AtomicUsize::new(0));
    let restored_by_probe = Arc::clone(&restored);
    let outcome = gate_reattempt::probe_most_overdue(
        &config(),
        &DefaultGatesConfig::default(),
        Duration::from_secs(1),
        move |workspace, _gates_run| async move {
            restored_by_probe.fetch_add(1, Ordering::Relaxed);
            gate_health::clear_state(&workspace)?;
            Ok(())
        },
    )
    .await?;
    assert!(matches!(
        outcome,
        IdleProbeOutcome::Probed {
            verdict: ProbeVerdict::Passed { gates_run: 0 },
            ..
        }
    ));
    assert_eq!(restored.load(Ordering::Relaxed), 1);

    degraded(workspace.path(), "fallback_clean_tree");
    std::fs::write(workspace.path().join("dirty.txt"), "uncommitted").unwrap();
    let outcome = gate_reattempt::probe_most_overdue(
        &config(),
        &DefaultGatesConfig::default(),
        Duration::from_secs(1),
        move |workspace, _gates_run| async move {
            gate_health::clear_state(&workspace)?;
            Ok(())
        },
    )
    .await?;
    assert!(matches!(
        outcome,
        IdleProbeOutcome::Probed {
            verdict: ProbeVerdict::Failed { .. },
            ..
        }
    ));
    assert_eq!(restored.load(Ordering::Relaxed), 1);
    assert!(gate_health::is_degraded(workspace.path())?);
    Ok(())
}

#[tokio::test]
#[serial_test::serial(nt_process_env)]
async fn unknown_no_gate_degradation_needs_operator() -> Result<()> {
    let (_guard, _fixture, workspace) = isolated_workspace();
    degraded(workspace.path(), "unknown_gate");
    let restored = Arc::new(AtomicUsize::new(0));
    let restored_by_probe = Arc::clone(&restored);

    let outcome = gate_reattempt::probe_most_overdue(
        &config(),
        &DefaultGatesConfig::default(),
        Duration::from_secs(1),
        move |workspace, _gates_run| async move {
            restored_by_probe.fetch_add(1, Ordering::Relaxed);
            gate_health::clear_state(&workspace)?;
            Ok(())
        },
    )
    .await?;

    assert!(matches!(
        outcome,
        IdleProbeOutcome::Probed {
            verdict: ProbeVerdict::NeedsOperator { .. },
            ..
        }
    ));
    assert_eq!(restored.load(Ordering::Relaxed), 0);
    assert!(gate_health::is_degraded(workspace.path())?);
    let schedule = gate_reattempt::load_state(workspace.path())?.unwrap();
    assert!(schedule.needs_operator());
    assert!(schedule
        .needs_operator_reason()
        .unwrap()
        .contains("unknown_gate"));
    Ok(())
}
