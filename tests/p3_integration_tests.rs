//! Integration tests for NEEDLE Phase 3 features.
//!
//! These tests exercise Phase 3 features end-to-end:
//! - Weave: gap analysis and bead creation from documentation
//! - Unravel: alternatives for HUMAN-blocked beads
//! - Pulse: codebase health scans
//! - Reflect: learning consolidation from closed beads
//! - Splice: failed-worker detection and escalation bead creation
//! - Knot: exhaustion diagnosis and starvation telemetry
//! - Validation gates: pre-closure verification
//! - Hook sink: telemetry dispatch to external commands
//! - Release channels: canary promote/reject/rollback
//! - Hot-reload: binary hash comparison and channel switching
//!
//! Each test uses isolated temporary workspaces for parallel safety.

#[path = "p3_integration_tests/alert_deduplication_test.rs"]
mod alert_deduplication_test;
#[path = "p3_integration_tests/alert_fingerprint_integration.rs"]
mod alert_fingerprint_integration;
#[path = "p3_integration_tests/anthropic_routing_e2e_test.rs"]
mod anthropic_routing_e2e_test;
#[path = "p3_integration_tests/anthropic_routing_verification.rs"]
mod anthropic_routing_verification;
#[path = "p3_integration_tests/binary_freshness_edge_cases.rs"]
mod binary_freshness_edge_cases;
#[path = "p3_integration_tests/binary_freshness_integration.rs"]
mod binary_freshness_integration;
#[path = "p3_integration_tests/binary_freshness_logging.rs"]
mod binary_freshness_logging;
#[path = "p3_integration_tests/default_routing_uses_builtin_adapters.rs"]
mod default_routing_uses_builtin_adapters;
#[path = "p3_integration_tests/dispatch_model_routing_validation.rs"]
mod dispatch_model_routing_validation;
#[path = "p3_integration_tests/end_to_end_telemetry_test.rs"]
mod end_to_end_telemetry_test;
#[path = "p3_integration_tests/file_sink_integration.rs"]
mod file_sink_integration;
#[path = "p3_integration_tests/gate_health_degradation_integration.rs"]
mod gate_health_degradation_integration;
#[path = "p3_integration_tests/github_release_upgrade_regression.rs"]
mod github_release_upgrade_regression;
#[path = "p3_integration_tests/immediate_check_trigger.rs"]
mod immediate_check_trigger;
#[path = "p3_integration_tests/interval_calculation.rs"]
mod interval_calculation;
#[path = "p3_integration_tests/long_lived_worker_binary_rotation.rs"]
mod long_lived_worker_binary_rotation;
#[path = "p3_integration_tests/manual_upgrade_path_tests.rs"]
mod manual_upgrade_path_tests;
#[path = "p3_integration_tests/otlp_integration.rs"]
mod otlp_integration;
#[path = "p3_integration_tests/otlp_runtime_test.rs"]
mod otlp_runtime_test;
#[path = "p3_integration_tests/otlp_transport_seam_tests.rs"]
mod otlp_transport_seam_tests;
#[path = "p3_integration_tests/post_dispatch_audit_test.rs"]
mod post_dispatch_audit_test;
#[path = "p3_integration_tests/query_integration_test.rs"]
mod query_integration_test;
#[path = "p3_integration_tests/routing_integration.rs"]
mod routing_integration;
#[path = "p3_integration_tests/routing_matcher_baseline.rs"]
mod routing_matcher_baseline;
#[path = "p3_integration_tests/routing_telemetry_verification.rs"]
mod routing_telemetry_verification;
#[path = "p3_integration_tests/starvation_tests.rs"]
mod starvation_tests;
#[path = "p3_integration_tests/supervisor_periodic_polling.rs"]
mod supervisor_periodic_polling;
#[path = "p3_integration_tests/telemetry_field_verification.rs"]
mod telemetry_field_verification;
#[path = "p3_integration_tests/test_otlp_config_syntax.rs"]
mod test_otlp_config_syntax;
#[path = "p3_integration_tests/test_panic_timestamp_verification.rs"]
mod test_panic_timestamp_verification;
#[path = "p3_integration_tests/timestamp_telemetry_tests.rs"]
mod timestamp_telemetry_tests;
#[path = "p3_integration_tests/upgrade_check_integration.rs"]
mod upgrade_check_integration;
#[path = "p3_integration_tests/verification_fingerprint_replay.rs"]
mod verification_fingerprint_replay;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use tempfile::TempDir;

use needle::bead_store::{builtin_bead_backends, BeadStore, CliBeadStore};
use needle::canary::CanaryRunner;
use needle::config::{
    HookConfig, KnotConfig, PulseConfig, ReflectConfig, ScannerConfig, SpliceConfig, UnravelConfig,
    WeaveConfig,
};
use needle::strand::pulse::PulseStrand;
use needle::strand::unravel::UnravelStrand;
use needle::strand::weave::WeaveStrand;
use needle::strand::{KnotStrand, ReflectStrand, SpliceStrand, Strand};
use needle::telemetry::{HookSink, Telemetry, TelemetryEvent};
use needle::types::{Bead, BeadId, BeadStatus, StrandResult};
use needle::upgrade::{check_hot_reload, file_hash, HotReloadCheck};
use needle::validation::ValidationGate;

// ═════════════════════════════════════════════════════════════════════════════
// Test infrastructure
// ═════════════════════════════════════════════════════════════════════════════

/// Path to a native bead-rs binary that is independent of operator HOME state.
///
/// The first `bead` on PATH may be a host queue-fence wrapper. Probe every
/// candidate with a disposable workspace and HOME so these fixtures retain
/// real CLI/database coverage without copying host policy into the test.
fn bead_path() -> PathBuf {
    static NATIVE_BEAD: OnceLock<PathBuf> = OnceLock::new();

    NATIVE_BEAD
        .get_or_init(|| {
            let mut candidates = Vec::new();
            if let Some(configured) = std::env::var_os("BEAD_RS_BIN") {
                candidates.push(PathBuf::from(configured));
            }
            if let Ok(paths) = which::which_all("bead") {
                candidates.extend(paths);
            }

            let mut seen = HashSet::new();
            for candidate in candidates {
                let identity = std::fs::canonicalize(&candidate).unwrap_or(candidate.clone());
                if !seen.insert(identity) || !candidate.is_file() {
                    continue;
                }

                let Ok(probe) = TempDir::new() else {
                    continue;
                };
                let workspace = probe.path().join("workspace");
                let home = probe.path().join("home");
                if fs::create_dir_all(&workspace).is_err() || fs::create_dir_all(&home).is_err() {
                    continue;
                }
                let usable = std::process::Command::new(&candidate)
                    .current_dir(&workspace)
                    .env("HOME", &home)
                    .args([
                        "init",
                        "--prefix",
                        "probe",
                        "--skip-foreign-workspace",
                        "--no-auto-flush",
                    ])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success());
                if usable {
                    return candidate;
                }
            }

            panic!(
                "a native bead-rs CLI must be installed; queue-fence wrappers requiring operator HOME are not valid test binaries"
            );
        })
        .clone()
}

fn bead_command(workspace: &Path) -> std::process::Command {
    let test_home = workspace.join(".test-home");
    std::fs::create_dir_all(&test_home).expect("failed to create isolated test HOME");
    let mut command = std::process::Command::new(bead_path());
    command.current_dir(workspace).env("HOME", test_home);
    command
}

/// Create an isolated test workspace with `.beads/` initialized.
fn create_test_workspace(prefix: &str) -> Result<TempDir> {
    let dir = tempfile::Builder::new()
        .prefix(&format!("needle-p3-{prefix}-"))
        .tempdir()
        .context("failed to create temp dir")?;

    let output = bead_command(dir.path())
        .args(["init", "--prefix", "p3", "--skip-foreign-workspace"])
        .output()
        .context("failed to run bead init")?;

    if !output.status.success() {
        anyhow::bail!(
            "bead init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let native_bead = serde_json::to_string(&bead_path())
        .context("failed to encode native bead path for workspace config")?;
    fs::write(
        dir.path().join(".needle.yaml"),
        format!("bead_cli:\n  backend: bead-rs\n  path: {native_bead}\n"),
    )
    .context("failed to bind test workspace to bead-rs")?;

    Ok(dir)
}

/// Create a bead in the test workspace and return its ID.
fn create_bead(workspace: &Path, title: &str) -> Result<BeadId> {
    let output = bead_command(workspace)
        .args(["create", "--title", title, "--description", title])
        .output()
        .context("failed to run bead create")?;

    if !output.status.success() {
        anyhow::bail!(
            "bead create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let id = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(BeadId::from(id))
}

/// Create a bead whose description exercises consumers of completed work.
fn create_bead_with_description(
    workspace: &Path,
    title: &str,
    description: &str,
) -> Result<BeadId> {
    let output = bead_command(workspace)
        .args(["create", "--title", title, "--description", description])
        .output()
        .context("failed to run bead create")?;

    if !output.status.success() {
        anyhow::bail!(
            "bead create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let id = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(BeadId::from(id))
}

/// Add a label to a bead.
fn add_label(workspace: &Path, bead_id: &BeadId, label: &str) -> Result<()> {
    let output = bead_command(workspace)
        .args(["label", "add", bead_id.as_ref(), "--label", label])
        .output()
        .context("failed to run bead label add")?;

    if !output.status.success() {
        anyhow::bail!(
            "bead label add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Get a bead store for a workspace.
fn store_for_workspace(workspace: &Path) -> Result<CliBeadStore> {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .ok_or_else(|| anyhow::anyhow!("built-in bead-rs descriptor missing"))?;
    CliBeadStore::new(
        backend,
        bead_path(),
        workspace.to_path_buf(),
        None,
        None,
        None,
    )
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

/// Build a minimal `Bead` for tests that need one without a live store.
fn make_test_bead(id: &str) -> Bead {
    Bead {
        id: BeadId::from(id.to_string()),
        title: format!("Test bead {id}"),
        body: Some("Do the thing".to_string()),
        priority: 1,
        status: BeadStatus::Open,
        assignee: None,
        labels: vec![],
        workspace: std::path::PathBuf::from("/tmp/test"),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
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
    let telemetry = Telemetry::with_log_dir(
        "test-weave".to_string(),
        &workspace.path().join(".needle/logs"),
    );
    let strand = WeaveStrand::new(
        config,
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        agent,
        telemetry,
    );

    let result = strand.evaluate(store.as_ref(), &HashSet::new()).await;
    assert!(
        matches!(result, StrandResult::WorkCreated),
        "weave should create work from agent findings, got {:?}",
        result
    );

    // Verify beads were created in the native bead-rs store.
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
    let telemetry = Telemetry::with_log_dir(
        "test-weave".to_string(),
        &workspace.path().join(".needle/logs"),
    );
    let strand = WeaveStrand::new(
        config,
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        agent,
        telemetry,
    );

    strand.evaluate(store.as_ref(), &HashSet::new()).await;

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
    let telemetry = Telemetry::with_log_dir(
        "test-weave".to_string(),
        &workspace.path().join(".needle/logs"),
    );
    let strand = WeaveStrand::new(
        config,
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        agent,
        telemetry,
    );

    let result = strand.evaluate(store.as_ref(), &HashSet::new()).await;
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
    let telemetry = Telemetry::with_log_dir(
        "test-unravel".to_string(),
        &workspace.path().join(".needle/logs"),
    );

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

    let result = strand.evaluate(store.as_ref(), &HashSet::new()).await;
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
    let telemetry = Telemetry::with_log_dir(
        "test-unravel-off".to_string(),
        &workspace.path().join(".needle/logs"),
    );

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

    let result = strand.evaluate(store.as_ref(), &HashSet::new()).await;
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
    let telemetry = Telemetry::with_log_dir(
        "test-pulse".to_string(),
        &workspace.path().join(".needle/logs"),
    );

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

    let result = strand.evaluate(store.as_ref(), &HashSet::new()).await;
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
    let telemetry1 = Telemetry::with_log_dir(
        "test-pulse-1".to_string(),
        &workspace.path().join(".needle/logs"),
    );
    let strand1 = PulseStrand::new(
        config.clone(),
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        telemetry1,
    );
    let result1 = strand1.evaluate(store.as_ref(), &HashSet::new()).await;
    assert!(matches!(result1, StrandResult::WorkCreated));

    let beads_after_first = store.list_all().await.unwrap().len();

    // Second scan — same issue, should NOT create a bead (dedup).
    let telemetry2 = Telemetry::with_log_dir(
        "test-pulse-2".to_string(),
        &workspace.path().join(".needle/logs"),
    );
    let strand2 = PulseStrand::new(
        config,
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        telemetry2,
    );
    let result2 = strand2.evaluate(store.as_ref(), &HashSet::new()).await;
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
// Test 4: Reflect — consolidates completed work into durable learnings
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn reflect_consolidates_closed_bead_into_learnings() {
    let workspace = create_test_workspace("reflect-consolidate").unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());
    let retrospective = r#"Implemented and verified.

## Retrospective
- **What worked:** Kept the fixture isolated in a temporary workspace
- **What didn't:** N/A
- **Surprise:** The strand outcome remains NoWork after consolidation
- **Reusable pattern:** Dispatch Reflect against the configured bead store"#;
    let bead_id = create_bead_with_description(
        workspace.path(),
        "Reflect integration test bead",
        retrospective,
    )
    .unwrap();

    let output = bead_command(workspace.path())
        .args([
            "close",
            bead_id.as_ref(),
            "--reason",
            "Completed with retrospective",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "bead close failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let config = ReflectConfig {
        enabled: true,
        min_beads_since_last: 1,
        cooldown_hours: 0,
        drift_enabled: false,
        adr_enabled: false,
        claude_md_placement: false,
        ..ReflectConfig::default()
    };
    let strand = ReflectStrand::new(
        config,
        workspace.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        Telemetry::new("test-reflect".to_string()),
        None,
    );

    let result = strand.evaluate(store.as_ref(), &HashSet::new()).await;
    assert!(
        matches!(result, StrandResult::NoWork),
        "reflect should finish consolidation and continue the waterfall, got {result:?}"
    );

    let learnings = fs::read_to_string(workspace.path().join(".beads/learnings.md"))
        .expect("reflect should persist workspace learnings");
    assert!(
        learnings.contains("Dispatch Reflect against the configured bead store"),
        "reflect should extract the reusable pattern from the closed bead"
    );
    assert!(
        state_dir.path().join("reflect_state.json").is_file(),
        "reflect should persist its consolidation watermark"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 5: Splice — documents a failed worker once
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn splice_documents_stale_worker_and_deduplicates_session() {
    let workspace = create_test_workspace("splice-failure").unwrap();
    let heartbeat_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());
    let session = format!(
        "needle-p3-splice-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_micros()
    );
    let heartbeat = serde_json::json!({
        "worker_id": "failed-integration-worker",
        "pid": std::process::id(),
        "state": "executing",
        "current_bead": "p3-stuck",
        "workspace": workspace.path(),
        "last_heartbeat": chrono::Utc::now() - chrono::Duration::hours(1),
        "session": session,
        "beads_processed": 3
    });
    fs::write(
        heartbeat_dir.path().join("failed-integration-worker.json"),
        serde_json::to_vec_pretty(&heartbeat).unwrap(),
    )
    .unwrap();

    let config = SpliceConfig {
        enabled: true,
        stale_threshold_secs: 1,
        report_workspace: Some(workspace.path().to_path_buf()),
        detect_live_loops: false,
        ..SpliceConfig::default()
    };
    let strand = SpliceStrand::new(
        config,
        heartbeat_dir.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        Telemetry::new("test-splice".to_string()),
    );

    let first = strand.evaluate(store.as_ref(), &HashSet::new()).await;
    assert!(
        matches!(first, StrandResult::WorkCreated),
        "splice should create escalation work for a stale worker, got {first:?}"
    );

    let beads_after_first = store.list_all().await.unwrap();
    assert_eq!(
        beads_after_first
            .iter()
            .filter(|bead| {
                bead.title
                    .contains("Worker failure: failed-integration-worker")
            })
            .count(),
        1,
        "splice should create exactly one worker-failure bead"
    );

    let second = strand.evaluate(store.as_ref(), &HashSet::new()).await;
    assert!(
        matches!(second, StrandResult::NoWork),
        "splice should deduplicate an already documented session, got {second:?}"
    );
    assert_eq!(
        store.list_all().await.unwrap().len(),
        beads_after_first.len(),
        "a repeated dispatch must not create another escalation bead"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 6: Knot — reports an invisible open queue through telemetry
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn knot_emits_starvation_telemetry_for_open_beads() {
    let workspace = create_test_workspace("knot-invisible").unwrap();
    create_bead(workspace.path(), "Open bead hidden from Pluck").unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());
    let log_dir = workspace.path().join(".needle/logs");
    let telemetry = Telemetry::with_log_dir("test-knot".to_string(), &log_dir);
    telemetry.start();
    let strand = KnotStrand::new(
        KnotConfig {
            exhaustion_threshold: 1,
            alert_cooldown_minutes: 60,
            // This test asserts the terminal verdict on the threshold cycle;
            // the transient-starvation backoff has its own unit tests in knot.rs.
            starvation_backoff_minutes: 0,
            ..KnotConfig::default()
        },
        telemetry.clone(),
    );

    let result = strand.evaluate(store.as_ref(), &HashSet::new()).await;
    assert!(
        matches!(result, StrandResult::NoWork),
        "knot should diagnose exhaustion without dispatching work, got {result:?}"
    );
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    telemetry.shutdown().await;

    let events: Vec<serde_json::Value> = fs::read_dir(&log_dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| fs::read_to_string(entry.path()).ok())
        .flat_map(|content| {
            content
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect::<Vec<_>>()
        })
        .collect();
    // Name the expected verdict explicitly: `strand.knot.starvation_detected`
    // is the sole, terminal starvation verdict (Knot-owned since 865484e4),
    // emitted on the threshold cycle itself because the config above disables
    // the backoff. A rename, a move back to Pluck, or a re-introduced emission
    // delay must fail loudly here rather than silently pass.
    let starvation = events
        .iter()
        .find(|event| event["event_type"] == "strand.knot.starvation_detected")
        .expect(
            "expected the terminal starvation verdict strand.knot.starvation_detected \
             on the threshold cycle — Knot has owned the sole verdict since 865484e4",
        );
    assert_eq!(starvation["data"]["open_count"], 1);
    assert!(
        events
            .iter()
            .all(|event| event["event_type"] != "strand.pluck.starvation_detected"),
        "strand.pluck.starvation_detected is retired — a second starvation verdict \
         means the strand waterfall grew a duplicate alert path"
    );
    assert_eq!(store.list_all().await.unwrap().len(), 1);
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 7: Validation gates — block closure on test failure
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn validation_gate_passes_all_commands() {
    let workspace = tempfile::tempdir().unwrap();

    let gate = ValidationGate::from_commands(
        vec![
            "true".to_string(),
            "echo ok".to_string(),
            "test -d /tmp".to_string(),
        ],
        workspace.path().to_path_buf(),
    )
    .unwrap();

    let bead = make_test_bead("gate-test-pass");
    let result = gate.run(&bead).await.unwrap();
    assert!(result.all_passed, "all gate commands should pass");
    assert!(result.results.values().all(|r| r.passed()));
}

#[tokio::test]
async fn validation_gate_blocks_on_failure() {
    let workspace = tempfile::tempdir().unwrap();

    let gate = ValidationGate::from_commands(
        vec![
            "true".to_string(),
            "exit 1".to_string(), // Fails
            "echo should-not-run".to_string(),
        ],
        workspace.path().to_path_buf(),
    )
    .unwrap();

    let bead = make_test_bead("gate-test-fail");
    let result = gate.run(&bead).await.unwrap();
    assert!(!result.all_passed, "gate should fail on failing command");
    let failures: Vec<_> = result.results.values().filter(|r| !r.passed()).collect();
    assert_eq!(failures.len(), 1);
}

#[tokio::test]
async fn validation_gate_runs_in_workspace_directory() {
    let workspace = tempfile::tempdir().unwrap();
    // Create a marker file in the workspace.
    fs::write(workspace.path().join("marker.txt"), "exists").unwrap();

    let gate = ValidationGate::from_commands(
        vec!["test -f marker.txt".to_string()],
        workspace.path().to_path_buf(),
    )
    .unwrap();

    let bead = make_test_bead("gate-test-workspace");
    let result = gate.run(&bead).await.unwrap();
    assert!(
        result.all_passed,
        "gate should run in workspace directory and find marker.txt"
    );
}

#[tokio::test]
async fn validation_gate_captures_stderr() {
    let workspace = tempfile::tempdir().unwrap();

    let gate = ValidationGate::from_commands(
        vec!["echo 'test failure detail' >&2; exit 1".to_string()],
        workspace.path().to_path_buf(),
    )
    .unwrap();

    let bead = make_test_bead("gate-test-stderr");
    let result = gate.run(&bead).await.unwrap();
    assert!(!result.all_passed);
    let failure_reasons: Vec<_> = result
        .results
        .values()
        .filter_map(|r| r.failure_reason())
        .collect();
    assert!(
        failure_reasons
            .iter()
            .any(|r| r.contains("test failure detail")),
        "gate should capture stderr output"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 8: Hook sink — delivers to configured command
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn hook_sink_dispatches_matching_events() {
    let output_file = tempfile::NamedTempFile::new().unwrap();
    let output_path = output_file.path().to_str().unwrap().to_string();

    let hooks = vec![HookConfig {
        event_filter: "outcome.*".to_string(),
        command: format!("cat >> {output_path}"),
        url: None,
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
        trace_id: None,
        span_id: None,
        attempt_id: None,
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
        url: None,
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
        trace_id: None,
        span_id: None,
        attempt_id: None,
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
        url: None,
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
        trace_id: None,
        span_id: None,
        attempt_id: None,
    };

    let failures = sink.dispatch(&error_event);
    assert!(
        failures.is_empty(),
        "sink_error events should be silently dropped to prevent recursion"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 9: Release channels — canary promote/reject/rollback
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
// Test 10: Hot-reload — binary hash comparison
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
// Test 11: Rollback — file_hash verifies integrity
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
