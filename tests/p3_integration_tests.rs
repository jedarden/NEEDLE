//! Integration tests for NEEDLE Phase 3 features.
//!
//! These tests exercise Phase 3 features end-to-end:
//! - Weave: gap analysis and bead creation from documentation
//! - Unravel: alternatives for HUMAN-blocked beads
//! - Pulse: codebase health scans
//! - Validation gates: pre-closure verification
//! - Hook sink: telemetry dispatch to external commands
//! - Release channels: canary promote/reject/rollback
//! - Hot-reload: binary hash comparison and channel switching
//!
//! Each test uses isolated temporary workspaces for parallel safety.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tempfile::TempDir;

use needle::bead_store::{BeadStore, BrCliBeadStore};
use needle::canary::CanaryRunner;
use needle::config::{HookConfig, PulseConfig, ScannerConfig, UnravelConfig, WeaveConfig};
use needle::strand::pulse::PulseStrand;
use needle::strand::unravel::UnravelStrand;
use needle::strand::weave::WeaveStrand;
use needle::strand::Strand;
use needle::telemetry::{HookSink, Telemetry, TelemetryEvent};
use needle::types::{BeadId, StrandResult};
use needle::upgrade::{check_hot_reload, file_hash, HotReloadCheck};
use needle::validation::ValidationGate;

// ═════════════════════════════════════════════════════════════════════════════
// Test infrastructure
// ═════════════════════════════════════════════════════════════════════════════

/// Path to the br binary.
fn br_path() -> PathBuf {
    which::which("br").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(format!("{home}/.local/bin/br"))
    })
}

/// Create an isolated test workspace with `.beads/` initialized.
fn create_test_workspace(prefix: &str) -> Result<TempDir> {
    let dir = tempfile::Builder::new()
        .prefix(&format!("needle-p3-{prefix}-"))
        .tempdir()
        .context("failed to create temp dir")?;

    let br = br_path();
    let output = std::process::Command::new(&br)
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .context("failed to run br init")?;

    if !output.status.success() {
        anyhow::bail!(
            "br init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(dir)
}

/// Create a bead in the test workspace and return its ID.
fn create_bead(workspace: &Path, title: &str) -> Result<BeadId> {
    let br = br_path();
    let output = std::process::Command::new(&br)
        .args(["create", "--title", title, "--body", title, "--silent"])
        .current_dir(workspace)
        .output()
        .context("failed to run br create")?;

    if !output.status.success() {
        anyhow::bail!(
            "br create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let id = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(BeadId::from(id))
}

/// Add a label to a bead.
fn add_label(workspace: &Path, bead_id: &BeadId, label: &str) -> Result<()> {
    let br = br_path();
    let output = std::process::Command::new(&br)
        .args(["label", "add", bead_id.as_ref(), label])
        .current_dir(workspace)
        .output()
        .context("failed to run br label add")?;

    if !output.status.success() {
        anyhow::bail!(
            "br label add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Get a bead store for a workspace.
fn store_for_workspace(workspace: &Path) -> Result<BrCliBeadStore> {
    BrCliBeadStore::new(br_path(), workspace.to_path_buf())
}

/// Mock WeaveAgent that returns fixed JSON.
struct MockWeaveAgent {
    response: String,
}

#[async_trait::async_trait]
impl needle::strand::weave::WeaveAgent for MockWeaveAgent {
    async fn analyze_gaps(&self, _prompt: &str, _workspace: &Path) -> Result<String> {
        Ok(self.response.clone())
    }
}

/// Mock UnravelAgent that returns fixed JSON.
struct MockUnravelAgent {
    response: String,
}

#[async_trait::async_trait]
impl needle::strand::unravel::UnravelAgent for MockUnravelAgent {
    async fn propose_alternatives(&self, _prompt: &str, _workspace: &Path) -> Result<String> {
        Ok(self.response.clone())
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 1: Weave — creates beads from doc gaps, respects guardrails
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn weave_creates_beads_from_agent_response() {
    let workspace = create_test_workspace("weave-create").unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());

    let agent_response = r#"[
        {"title": "Add error handling to auth module", "body": "The auth module lacks proper error handling for expired tokens.", "priority": 2},
        {"title": "Document API endpoints", "body": "REST API endpoints are undocumented.", "priority": 3}
    ]"#;

    // Weave requires doc files in workspace to analyze.
    fs::write(
        workspace.path().join("README.md"),
        "# Test Project\n\nA sample project for gap analysis testing.\n",
    )
    .unwrap();

    let config = WeaveConfig {
        enabled: true,
        max_beads_per_run: 5,
        cooldown_hours: 0,
        ..WeaveConfig::default()
    };

    let agent = Box::new(MockWeaveAgent {
        response: agent_response.to_string(),
    });
    let strand = WeaveStrand::new(
        config,
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        agent,
    );

    let result = strand.evaluate(store.as_ref()).await;
    assert!(
        matches!(result, StrandResult::WorkCreated),
        "weave should create work from agent findings, got {:?}",
        result
    );

    // Verify beads were created in the real br store.
    let all_beads = store.list_all().await.unwrap();
    let weave_beads: Vec<_> = all_beads
        .iter()
        .filter(|b| b.title.contains("Add error handling") || b.title.contains("Document API"))
        .collect();
    assert!(
        weave_beads.len() >= 2,
        "expected at least 2 weave-created beads, got {}",
        weave_beads.len()
    );
}

#[tokio::test]
async fn weave_respects_max_beads_guardrail() {
    let workspace = create_test_workspace("weave-max").unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());

    // Weave requires doc files in workspace to analyze.
    fs::write(
        workspace.path().join("README.md"),
        "# Test\n\nSample project.\n",
    )
    .unwrap();

    // Agent proposes 5 beads but max is 2.
    let agent_response = r#"[
        {"title": "Bead one", "body": "First", "priority": 3},
        {"title": "Bead two", "body": "Second", "priority": 3},
        {"title": "Bead three", "body": "Third", "priority": 3},
        {"title": "Bead four", "body": "Fourth", "priority": 3},
        {"title": "Bead five", "body": "Fifth", "priority": 3}
    ]"#;

    let config = WeaveConfig {
        enabled: true,
        max_beads_per_run: 2,
        cooldown_hours: 0,
        ..WeaveConfig::default()
    };

    let agent = Box::new(MockWeaveAgent {
        response: agent_response.to_string(),
    });
    let strand = WeaveStrand::new(
        config,
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        agent,
    );

    strand.evaluate(store.as_ref()).await;

    let all_beads = store.list_all().await.unwrap();
    assert!(
        all_beads.len() <= 2,
        "max_beads_per_run=2 should limit creation, got {} beads",
        all_beads.len()
    );
}

#[tokio::test]
async fn weave_disabled_returns_no_work() {
    let workspace = create_test_workspace("weave-off").unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());

    let config = WeaveConfig::default(); // disabled by default

    let agent = Box::new(MockWeaveAgent {
        response: "should not be called".to_string(),
    });
    let strand = WeaveStrand::new(
        config,
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        agent,
    );

    let result = strand.evaluate(store.as_ref()).await;
    assert!(
        matches!(result, StrandResult::NoWork),
        "disabled weave should return NoWork"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 2: Unravel — proposes alternatives, doesn't modify originals
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn unravel_creates_alternatives_without_modifying_original() {
    let workspace = create_test_workspace("unravel-alt").unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());
    let telemetry = Telemetry::new("test-unravel".to_string());

    // Create a bead and label it as human-blocked (lowercase per filter_human_beads).
    let bead_id = create_bead(workspace.path(), "Human-blocked: need API key from vendor").unwrap();
    add_label(workspace.path(), &bead_id, "human").unwrap();

    let agent_response = r#"[
        {"title": "Use mock API key for testing", "body": "Create a mock provider that simulates the vendor API."},
        {"title": "Use environment variable fallback", "body": "Allow API key to be loaded from env var."}
    ]"#;

    let config = UnravelConfig {
        enabled: true,
        max_beads_per_run: 5,
        max_alternatives_per_bead: 3,
        cooldown_hours: 0,
        ..UnravelConfig::default()
    };

    let agent = Box::new(MockUnravelAgent {
        response: agent_response.to_string(),
    });
    let strand = UnravelStrand::new(
        config,
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        agent,
        telemetry,
    );

    let result = strand.evaluate(store.as_ref()).await;
    assert!(
        matches!(result, StrandResult::WorkCreated),
        "unravel should create alternative beads, got {:?}",
        result
    );

    // Verify original bead is still open and unmodified.
    let original = store.show(&bead_id).await.unwrap();
    assert!(
        original.title.contains("Human-blocked"),
        "original bead title should be unmodified"
    );

    // Verify alternatives were created.
    let all_beads = store.list_all().await.unwrap();
    let alternatives: Vec<_> = all_beads
        .iter()
        .filter(|b| b.title.contains("mock API") || b.title.contains("environment variable"))
        .collect();
    assert!(
        alternatives.len() >= 2,
        "expected at least 2 alternatives, got {}",
        alternatives.len()
    );
}

#[tokio::test]
async fn unravel_disabled_returns_no_work() {
    let workspace = create_test_workspace("unravel-off").unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());
    let telemetry = Telemetry::new("test-unravel-off".to_string());

    let config = UnravelConfig::default(); // disabled by default

    let agent = Box::new(MockUnravelAgent {
        response: "should not be called".to_string(),
    });
    let strand = UnravelStrand::new(
        config,
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        agent,
        telemetry,
    );

    let result = strand.evaluate(store.as_ref()).await;
    assert!(
        matches!(result, StrandResult::NoWork),
        "disabled unravel should return NoWork"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 3: Pulse — detects issues, deduplicates across scans
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn pulse_detects_scanner_findings_and_creates_beads() {
    let workspace = create_test_workspace("pulse-detect").unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());
    let telemetry = Telemetry::new("test-pulse".to_string());

    let config = PulseConfig {
        enabled: true,
        scanners: vec![ScannerConfig {
            name: "test-scanner".to_string(),
            command: "echo 'src/main.rs:10:1: error: unused import std::io'".to_string(),
            severity_threshold: None,
        }],
        cooldown_hours: 0,
        severity_threshold: 5, // Accept all severities
        max_beads_per_run: 10,
        ..PulseConfig::default()
    };

    let strand = PulseStrand::new(
        config,
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        telemetry,
    );

    let result = strand.evaluate(store.as_ref()).await;
    assert!(
        matches!(result, StrandResult::WorkCreated),
        "pulse should create beads from scanner findings, got {:?}",
        result
    );

    let all_beads = store.list_all().await.unwrap();
    let pulse_beads: Vec<_> = all_beads
        .iter()
        .filter(|b| b.title.contains("[Pulse]"))
        .collect();
    assert!(
        !pulse_beads.is_empty(),
        "expected at least 1 pulse-created bead"
    );
}

#[tokio::test]
async fn pulse_deduplicates_across_scans() {
    let workspace = create_test_workspace("pulse-dedup").unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());

    let config = PulseConfig {
        enabled: true,
        scanners: vec![ScannerConfig {
            name: "dedup-scanner".to_string(),
            command: "echo 'error: same issue every time'".to_string(),
            severity_threshold: None,
        }],
        cooldown_hours: 0,
        severity_threshold: 5,
        max_beads_per_run: 10,
        ..PulseConfig::default()
    };

    // First scan — should create a bead.
    let telemetry1 = Telemetry::new("test-pulse-1".to_string());
    let strand1 = PulseStrand::new(
        config.clone(),
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        telemetry1,
    );
    let result1 = strand1.evaluate(store.as_ref()).await;
    assert!(matches!(result1, StrandResult::WorkCreated));

    let beads_after_first = store.list_all().await.unwrap().len();

    // Second scan — same issue, should NOT create a bead (dedup).
    let telemetry2 = Telemetry::new("test-pulse-2".to_string());
    let strand2 = PulseStrand::new(
        config,
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        telemetry2,
    );
    let result2 = strand2.evaluate(store.as_ref()).await;
    assert!(
        matches!(result2, StrandResult::NoWork),
        "second scan should return NoWork (dedup), got {:?}",
        result2
    );

    let beads_after_second = store.list_all().await.unwrap().len();
    assert_eq!(
        beads_after_first, beads_after_second,
        "no new beads should be created on duplicate findings"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 4: Validation gates — block closure on test failure
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn validation_gate_passes_all_commands() {
    let workspace = tempfile::tempdir().unwrap();

    let gate = ValidationGate::new(
        vec![
            "true".to_string(),
            "echo ok".to_string(),
            "test -d /tmp".to_string(),
        ],
        workspace.path().to_path_buf(),
    )
    .unwrap();

    let result = gate.run().await.unwrap();
    assert!(result.passed, "all gate commands should pass");
    assert!(result.failures.is_empty());
}

#[tokio::test]
async fn validation_gate_blocks_on_failure() {
    let workspace = tempfile::tempdir().unwrap();

    let gate = ValidationGate::new(
        vec![
            "true".to_string(),
            "exit 1".to_string(), // Fails
            "echo should-not-run".to_string(),
        ],
        workspace.path().to_path_buf(),
    )
    .unwrap();

    let result = gate.run().await.unwrap();
    assert!(!result.passed, "gate should fail on failing command");
    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0].exit_code, Some(1));
}

#[tokio::test]
async fn validation_gate_runs_in_workspace_directory() {
    let workspace = tempfile::tempdir().unwrap();
    // Create a marker file in the workspace.
    fs::write(workspace.path().join("marker.txt"), "exists").unwrap();

    let gate = ValidationGate::new(
        vec!["test -f marker.txt".to_string()],
        workspace.path().to_path_buf(),
    )
    .unwrap();

    let result = gate.run().await.unwrap();
    assert!(
        result.passed,
        "gate should run in workspace directory and find marker.txt"
    );
}

#[tokio::test]
async fn validation_gate_captures_stderr() {
    let workspace = tempfile::tempdir().unwrap();

    let gate = ValidationGate::new(
        vec!["echo 'test failure detail' >&2; exit 1".to_string()],
        workspace.path().to_path_buf(),
    )
    .unwrap();

    let result = gate.run().await.unwrap();
    assert!(!result.passed);
    assert!(
        result.failures[0].output.contains("test failure detail"),
        "gate should capture stderr output"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 5: Hook sink — delivers to configured command
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn hook_sink_dispatches_matching_events() {
    let output_file = tempfile::NamedTempFile::new().unwrap();
    let output_path = output_file.path().to_str().unwrap().to_string();

    let hooks = vec![HookConfig {
        event_filter: "outcome.*".to_string(),
        command: format!("cat >> {output_path}"),
    }];

    let sink = HookSink::new(&hooks).unwrap();
    assert!(!sink.is_empty());

    // Create a matching event.
    let event = TelemetryEvent {
        timestamp: chrono::Utc::now(),
        event_type: "outcome.success".to_string(),
        worker_id: "test-worker".to_string(),
        session_id: "test-session".to_string(),
        sequence: 1,
        bead_id: None,
        workspace: None,
        data: serde_json::json!({"bead_id": "test-123"}),
        duration_ms: None,
    };

    let failures = sink.dispatch(&event);
    assert!(
        failures.is_empty(),
        "dispatch should succeed without errors"
    );

    // Give the hook time to execute (fire-and-forget).
    std::thread::sleep(std::time::Duration::from_millis(200));

    let output = fs::read_to_string(output_file.path()).unwrap();
    assert!(
        output.contains("outcome.success"),
        "hook should receive the event JSON, got: {output}"
    );
}

#[test]
fn hook_sink_skips_non_matching_events() {
    let hooks = vec![HookConfig {
        event_filter: "outcome.*".to_string(),
        command: "echo should-not-run".to_string(),
    }];

    let sink = HookSink::new(&hooks).unwrap();

    // Create a non-matching event.
    let event = TelemetryEvent {
        timestamp: chrono::Utc::now(),
        event_type: "worker.started".to_string(),
        worker_id: "test-worker".to_string(),
        session_id: "test-session".to_string(),
        sequence: 1,
        bead_id: None,
        workspace: None,
        data: serde_json::json!({}),
        duration_ms: None,
    };

    let failures = sink.dispatch(&event);
    assert!(
        failures.is_empty(),
        "non-matching event should not produce errors"
    );
}

#[test]
fn hook_sink_prevents_recursion_on_sink_errors() {
    let hooks = vec![HookConfig {
        event_filter: "*".to_string(),
        command: "cat".to_string(),
    }];

    let sink = HookSink::new(&hooks).unwrap();

    // A sink error event should never be dispatched to hooks (recursion prevention).
    let error_event = TelemetryEvent {
        timestamp: chrono::Utc::now(),
        event_type: "telemetry.sink_error".to_string(),
        worker_id: "test-worker".to_string(),
        session_id: "test-session".to_string(),
        sequence: 1,
        bead_id: None,
        workspace: None,
        data: serde_json::json!({"error": "hook failed"}),
        duration_ms: None,
    };

    let failures = sink.dispatch(&error_event);
    assert!(
        failures.is_empty(),
        "sink_error events should be silently dropped to prevent recursion"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 6: Release channels — canary promote/reject/rollback
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn canary_promote_moves_testing_to_stable() {
    let home = tempfile::tempdir().unwrap();
    let canary_ws = tempfile::tempdir().unwrap();
    let bin_dir = home.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    // Create a testing binary.
    let testing_binary = bin_dir.join("needle-testing");
    fs::write(&testing_binary, b"new binary v2.0.0").unwrap();

    // Create an existing stable binary.
    let stable_binary = bin_dir.join("needle-stable");
    fs::write(&stable_binary, b"old binary v1.0.0").unwrap();

    let runner = CanaryRunner::new(
        home.path().to_path_buf(),
        canary_ws.path().to_path_buf(),
        30,
    );

    // Promote testing → stable.
    runner.promote().unwrap();

    // Verify: testing content is now in stable.
    let stable_content = fs::read_to_string(runner.stable_binary()).unwrap();
    assert_eq!(
        stable_content, "new binary v2.0.0",
        "stable should contain testing binary content"
    );

    // Verify: old stable was backed up to .prev.
    let prev_content = fs::read_to_string(runner.prev_binary()).unwrap();
    assert_eq!(
        prev_content, "old binary v1.0.0",
        "prev should contain old stable binary content"
    );

    // Verify: testing binary is removed.
    assert!(
        !runner.testing_binary().exists(),
        "testing binary should be removed after promote"
    );
}

#[test]
fn canary_reject_removes_testing_binary() {
    let home = tempfile::tempdir().unwrap();
    let canary_ws = tempfile::tempdir().unwrap();
    let bin_dir = home.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let testing_binary = bin_dir.join("needle-testing");
    fs::write(&testing_binary, b"rejected binary").unwrap();

    let runner = CanaryRunner::new(
        home.path().to_path_buf(),
        canary_ws.path().to_path_buf(),
        30,
    );

    runner.reject().unwrap();

    assert!(
        !runner.testing_binary().exists(),
        "testing binary should be removed after reject"
    );
}

#[test]
fn canary_rollback_restores_previous_stable() {
    let home = tempfile::tempdir().unwrap();
    let canary_ws = tempfile::tempdir().unwrap();
    let bin_dir = home.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    // Create current stable and previous stable.
    let stable_binary = bin_dir.join("needle-stable");
    fs::write(&stable_binary, b"broken v2.0.0").unwrap();

    let prev_binary = bin_dir.join("needle-stable.prev");
    fs::write(&prev_binary, b"working v1.0.0").unwrap();

    let runner = CanaryRunner::new(
        home.path().to_path_buf(),
        canary_ws.path().to_path_buf(),
        30,
    );

    runner.rollback().unwrap();

    let stable_content = fs::read_to_string(runner.stable_binary()).unwrap();
    assert_eq!(
        stable_content, "working v1.0.0",
        "rollback should restore previous stable"
    );
}

#[test]
fn canary_status_reports_channel_state() {
    let home = tempfile::tempdir().unwrap();
    let canary_ws = tempfile::tempdir().unwrap();
    let bin_dir = home.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    // Create testing and stable binaries.
    fs::write(bin_dir.join("needle-testing"), b"testing").unwrap();
    fs::write(bin_dir.join("needle-stable"), b"stable").unwrap();

    let runner = CanaryRunner::new(
        home.path().to_path_buf(),
        canary_ws.path().to_path_buf(),
        30,
    );

    let status = runner.status().unwrap();
    assert!(status.testing_exists, "testing binary should exist");
    assert!(status.stable_exists, "stable binary should exist");
    assert!(!status.prev_exists, "prev binary should not exist");
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 7: Hot-reload — binary hash comparison
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn hot_reload_detects_new_stable_binary() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = home.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    // Write a different file as :stable.
    let stable = bin_dir.join("needle-stable");
    fs::write(&stable, b"completely different binary content").unwrap();

    let result = check_hot_reload(home.path()).unwrap();
    match result {
        HotReloadCheck::NewBinaryDetected {
            old_hash,
            new_hash,
            stable_path,
        } => {
            assert_ne!(old_hash, new_hash, "hashes should differ");
            assert_eq!(stable_path, stable);
        }
        other => panic!("expected NewBinaryDetected, got {:?}", other),
    }
}

#[test]
fn hot_reload_no_stable_returns_skipped() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join("bin")).unwrap();

    let result = check_hot_reload(home.path()).unwrap();
    assert!(
        matches!(result, HotReloadCheck::Skipped { .. }),
        "no stable binary should return Skipped"
    );
}

#[test]
fn hot_reload_same_binary_returns_no_change() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = home.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    // Copy current binary as :stable.
    let current_exe = std::env::current_exe().unwrap();
    let stable = bin_dir.join("needle-stable");
    fs::copy(&current_exe, &stable).unwrap();

    let result = check_hot_reload(home.path()).unwrap();
    assert_eq!(
        result,
        HotReloadCheck::NoChange,
        "same binary should return NoChange"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 8: Rollback — file_hash verifies integrity
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn file_hash_verifies_binary_integrity() {
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("binary-a");
    let file_b = dir.path().join("binary-b");

    fs::write(&file_a, b"binary content version 1").unwrap();
    fs::write(&file_b, b"binary content version 1").unwrap();

    let hash_a = file_hash(&file_a).unwrap();
    let hash_b = file_hash(&file_b).unwrap();

    // Same content should produce same hash.
    assert_eq!(hash_a, hash_b);
    assert_eq!(hash_a.len(), 64, "SHA-256 hex should be 64 chars");

    // Different content should produce different hash.
    fs::write(&file_b, b"binary content version 2").unwrap();
    let hash_b2 = file_hash(&file_b).unwrap();
    assert_ne!(hash_a, hash_b2);
}
