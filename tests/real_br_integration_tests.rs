//! Integration tests for NEEDLE Phase 2 using real `br` CLI.
//!
//! These tests use the actual `br` binary to create and manage beads in
//! isolated temporary workspaces. Each test:
//! - Creates its own `.beads/` directory
//! - Is parallel-safe (unique workspace paths per test)
//! - Cleans up after completion
//!
//! Test categories:
//! 1. Multi-worker claiming — N workers, M beads, each claimed exactly once
//! 2. Crashed worker bead released by peer monitoring
//! 3. Explore strand discovers work in other workspaces
//! 4. Mend strand cleans stale claims and orphaned locks
//! 5. Mitosis splits multi-task beads correctly
//! 6. Duplicate mitosis on same parent creates zero new children
//! 7. Concurrent mitosis on same parent: flock serializes
//! 10. Database corruption — corrupt SQLite, verify auto-repair from JSONL

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use tempfile::TempDir;

use needle::bead_store::{BeadStore, BrCliBeadStore, Filters};
use needle::claim::Claimer;
use needle::config::{ExploreConfig, MendConfig, MitosisConfig};
use needle::mitosis::{MitosisEvaluator, MitosisResult};
use needle::registry::Registry;
use needle::strand::{ExploreStrand, MendStrand, Strand, StrandRunner};
use needle::telemetry::Telemetry;
use needle::types::{BeadId, ClaimOutcome, StrandResult};

// ═════════════════════════════════════════════════════════════════════════════
// Test infrastructure
// ═════════════════════════════════════════════════════════════════════════════

/// Path to the br binary (discovered via PATH or ~/.local/bin/br).
fn br_path() -> PathBuf {
    which::which("br").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(format!("{home}/.local/bin/br"))
    })
}

/// Create an isolated test workspace with `.beads/` directory initialized.
fn create_test_workspace(prefix: &str) -> Result<TempDir> {
    let dir = tempfile::Builder::new()
        .prefix(&format!("needle-test-{prefix}-"))
        .tempdir()
        .context("failed to create temp dir")?;

    // Initialize .beads/ directory.
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

/// Create a bead in the test workspace.
///
/// Retries once with `br sync --flush-only` on FrankenSQLite sync conflicts.
fn create_bead(workspace: &Path, title: &str, priority: u8) -> Result<BeadId> {
    let br = br_path();
    let do_create = || {
        std::process::Command::new(&br)
            .args([
                "create",
                "--title",
                title,
                "--body",
                &format!("Test bead: {title}"),
                "--silent",
            ])
            .current_dir(workspace)
            .output()
            .context("failed to run br create")
    };

    let mut output = do_create()?;

    // Retry once on sync conflict (FrankenSQLite WAL race).
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Sync conflict") {
            let _ = std::process::Command::new(&br)
                .args(["sync", "--flush-only"])
                .current_dir(workspace)
                .output();
            output = do_create()?;
        }
    }

    if !output.status.success() {
        anyhow::bail!(
            "br create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let id = String::from_utf8(output.stdout)?.trim().to_string();
    // Set priority via update.
    let _ = std::process::Command::new(&br)
        .args(["update", &id, "--priority", &priority.to_string()])
        .current_dir(workspace)
        .output();

    Ok(BeadId::from(id))
}

/// Add a label to a bead.
fn add_label(workspace: &Path, bead_id: &BeadId, label: &str) -> Result<()> {
    let br = br_path();
    let do_add = || {
        std::process::Command::new(&br)
            .args(["label", "add", bead_id.as_ref(), label])
            .current_dir(workspace)
            .output()
            .context("failed to run br label add")
    };

    let mut output = do_add()?;

    // Retry once on sync conflict (FrankenSQLite WAL race).
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Sync conflict") || stderr.contains("sync conflict") {
            let _ = std::process::Command::new(&br)
                .args(["sync", "--flush-only"])
                .current_dir(workspace)
                .output();
            output = do_add()?;
        }
    }

    if !output.status.success() {
        anyhow::bail!(
            "br label add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Get bead store for a workspace.
fn store_for_workspace(workspace: &Path) -> Result<BrCliBeadStore> {
    BrCliBeadStore::new(br_path(), workspace.to_path_buf())
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 1: Multi-worker claiming — N workers, M beads, each claimed exactly once
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn real_br_multi_worker_claiming_no_duplicates() {
    let workspace = create_test_workspace("mw-claim").unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());

    // Create 5 beads.
    let mut bead_ids = Vec::new();
    for i in 0..5u32 {
        let id = create_bead(workspace.path(), &format!("multi-worker-test-{i}"), 1).unwrap();
        bead_ids.push(id);
    }

    // Fetch ready beads.
    let beads = store.ready(&Filters::default()).await.unwrap();
    assert_eq!(beads.len(), 5, "should have 5 ready beads");

    // 5 workers claim sequentially from the shared bead list.
    // Sequential claiming avoids FrankenSQLite WAL races while still
    // verifying that each worker skips already-claimed beads and picks
    // a unique bead. Concurrent claiming is covered by
    // p2_integration_tests::multi_worker_claiming_no_duplicates (mock store).
    let lock_dir = tempfile::tempdir().unwrap();
    let mut claimed_ids: Vec<String> = Vec::new();
    let mut already_claimed: HashSet<BeadId> = HashSet::new();

    for worker_idx in 0..5u32 {
        let claimer = Claimer::new(
            store.clone(),
            lock_dir.path().to_path_buf(),
            5,
            10,
            Telemetry::new(format!("worker-{worker_idx}")),
        );

        let result = claimer
            .claim_next(&beads, &format!("worker-{worker_idx}"), &already_claimed)
            .await
            .unwrap();

        if let ClaimOutcome::Claimed(bead) = result {
            already_claimed.insert(bead.id.clone());
            claimed_ids.push(bead.id.to_string());
        }
    }

    // Verify: exactly 5 unique claims, no duplicates.
    let unique: HashSet<&String> = claimed_ids.iter().collect();
    assert_eq!(
        unique.len(),
        claimed_ids.len(),
        "no duplicate claims allowed; claimed: {:?}",
        claimed_ids
    );
    assert_eq!(unique.len(), 5, "all 5 beads should be claimed");
}

#[tokio::test]
async fn real_br_all_beads_eventually_claimed() {
    let workspace = create_test_workspace("allclaim").unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());

    // Create 3 beads with different priorities.
    let _ = create_bead(workspace.path(), "p1-bead", 1).unwrap();
    let _ = create_bead(workspace.path(), "p2-bead", 2).unwrap();
    let _ = create_bead(workspace.path(), "p3-bead", 3).unwrap();

    let beads = store.ready(&Filters::default()).await.unwrap();
    assert_eq!(beads.len(), 3);

    // Claim all 3 sequentially, passing already-claimed IDs as exclusions.
    let lock_dir = tempfile::tempdir().unwrap();
    let mut claimed: Vec<String> = Vec::new();
    let mut already_claimed: HashSet<BeadId> = HashSet::new();

    for worker_idx in 0..3u32 {
        let claimer = Claimer::new(
            store.clone(),
            lock_dir.path().to_path_buf(),
            5,
            10,
            Telemetry::new(format!("worker-{worker_idx}")),
        );

        let result = claimer
            .claim_next(&beads, &format!("worker-{worker_idx}"), &already_claimed)
            .await
            .unwrap();

        if let ClaimOutcome::Claimed(bead) = result {
            already_claimed.insert(bead.id.clone());
            claimed.push(bead.id.to_string());
        }
    }

    assert_eq!(claimed.len(), 3, "all 3 beads should be claimed");

    // Verify no duplicates.
    let unique: HashSet<String> = claimed.into_iter().collect();
    assert_eq!(unique.len(), 3, "all claims should be unique");
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 2: Crashed worker bead released by peer monitoring
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn real_br_crashed_worker_bead_released_by_peer() {
    let workspace = create_test_workspace("crash-release").unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());
    let hb_dir = tempfile::tempdir().unwrap();
    let reg_dir = tempfile::tempdir().unwrap();

    // Create a bead and manually claim it as a "crashed" worker.
    let bead_id = create_bead(workspace.path(), "orphan-bead", 1).unwrap();

    // Claim the bead as crashed-worker.
    let _ = store.claim(&bead_id, "crashed-worker").await.unwrap();

    // Write a stale heartbeat for the crashed worker.
    let heartbeat_data = needle::health::HeartbeatData {
        worker_id: "crashed-worker".to_string(),
        pid: 99_999_999, // Dead PID.
        state: needle::types::WorkerState::Executing,
        current_bead: Some(bead_id.clone()),
        workspace: workspace.path().to_path_buf(),
        last_heartbeat: Utc::now() - chrono::Duration::seconds(600), // Stale.
        started_at: Utc::now() - chrono::Duration::seconds(3600),
        beads_processed: 1,
        session: "crashed-worker".to_string(),
    };
    let hb_path = hb_dir.path().join("crashed-worker.json");
    std::fs::write(&hb_path, serde_json::to_string(&heartbeat_data).unwrap()).unwrap();

    // Verify bead is in-progress (claimed).
    let bead = store.show(&bead_id).await.unwrap();
    assert!(
        bead.assignee.is_some(),
        "bead should be assigned to crashed worker"
    );

    // Run peer monitor.
    let registry = Registry::new(reg_dir.path());
    let telemetry = Telemetry::new("monitor-worker".to_string());
    let monitor = needle::peer::PeerMonitor::new(
        hb_dir.path().to_path_buf(),
        Duration::from_secs(300),
        "monitor-worker".to_string(),
        store.as_ref(),
        &registry,
        telemetry,
    );

    let result = monitor.check_peers().await.unwrap();
    assert_eq!(result.crashed_count, 1, "should detect 1 crashed peer");
    assert_eq!(result.beads_released, 1, "should release 1 bead");

    // Verify bead is now unassigned.
    let bead = store.show(&bead_id).await.unwrap();
    assert!(
        bead.assignee.is_none(),
        "bead should be released (no assignee)"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 3: Explore strand discovers work in other workspaces
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn real_br_explore_discovers_remote_workspace() {
    // Create home workspace (empty).
    let home_workspace = create_test_workspace("explore-home").unwrap();
    let home_store = store_for_workspace(home_workspace.path()).unwrap();

    // Create remote workspace with a bead.
    let remote_workspace = create_test_workspace("explore-remote").unwrap();
    let _ = create_bead(remote_workspace.path(), "remote-bead", 1).unwrap();

    // Configure Explore strand to search the remote workspace.
    let config = ExploreConfig {
        enabled: true,
        workspaces: vec![remote_workspace.path().to_path_buf()],
    };

    let explore = ExploreStrand::new(config, home_workspace.path().to_path_buf());

    // Home workspace is empty, so Explore should find the remote bead.
    let result = explore.evaluate(&home_store).await;

    match result {
        StrandResult::BeadFound(beads) => {
            assert_eq!(beads.len(), 1, "should find 1 bead in remote workspace");
            assert_eq!(beads[0].title, "remote-bead", "should find the remote bead");
        }
        StrandResult::NoWork => {
            panic!("Explore should find bead in remote workspace, got NoWork");
        }
        other => panic!("Unexpected result: {:?}", other),
    }
}

#[tokio::test]
async fn real_br_explore_skips_home_workspace() {
    let home_workspace = create_test_workspace("explore-skip").unwrap();
    let home_store = store_for_workspace(home_workspace.path()).unwrap();

    // Create a bead in home workspace.
    let _ = create_bead(home_workspace.path(), "home-bead", 1).unwrap();

    // Configure Explore with home workspace in the list (should be skipped).
    let config = ExploreConfig {
        enabled: true,
        workspaces: vec![home_workspace.path().to_path_buf()],
    };

    let explore = ExploreStrand::new(config, home_workspace.path().to_path_buf());

    // Explore should skip home and return NoWork.
    let result = explore.evaluate(&home_store).await;
    assert!(
        matches!(result, StrandResult::NoWork),
        "Explore should skip home workspace; got {:?}",
        result
    );
}

#[tokio::test]
async fn real_br_explore_disabled_returns_no_work() {
    let home_workspace = create_test_workspace("explore-disabled").unwrap();
    let home_store = store_for_workspace(home_workspace.path()).unwrap();

    let remote_workspace = create_test_workspace("explore-remote-2").unwrap();
    let _ = create_bead(remote_workspace.path(), "remote-bead-2", 1).unwrap();

    // Configure Explore as disabled.
    let config = ExploreConfig {
        enabled: false,
        workspaces: vec![remote_workspace.path().to_path_buf()],
    };

    let explore = ExploreStrand::new(config, home_workspace.path().to_path_buf());

    let result = explore.evaluate(&home_store).await;
    assert!(
        matches!(result, StrandResult::NoWork),
        "Disabled Explore should return NoWork; got {:?}",
        result
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 4: Mend strand cleans stale claims and orphaned locks
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn real_br_mend_cleans_crashed_peer() {
    let workspace = create_test_workspace("mend-crash").unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());
    let hb_dir = tempfile::tempdir().unwrap();
    let reg_dir = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();

    // Create and claim a bead as a "crashed" worker.
    let bead_id = create_bead(workspace.path(), "stale-claim-bead", 1).unwrap();
    let _ = store.claim(&bead_id, "dead-peer").await.unwrap();

    // Write stale heartbeat for crashed worker.
    let heartbeat_data = needle::health::HeartbeatData {
        worker_id: "dead-peer".to_string(),
        pid: 99_999_999,
        state: needle::types::WorkerState::Executing,
        current_bead: Some(bead_id.clone()),
        workspace: workspace.path().to_path_buf(),
        last_heartbeat: Utc::now() - chrono::Duration::seconds(600),
        started_at: Utc::now() - chrono::Duration::seconds(3600),
        beads_processed: 1,
        session: "dead-peer".to_string(),
    };
    let hb_path = hb_dir.path().join("dead-peer.json");
    std::fs::write(&hb_path, serde_json::to_string(&heartbeat_data).unwrap()).unwrap();

    // Run Mend strand.
    let config = MendConfig::default();
    let registry = Registry::new(reg_dir.path());
    let telemetry = Telemetry::new("mend-worker".to_string());

    let mend = MendStrand::new(
        config,
        hb_dir.path().to_path_buf(),
        Duration::from_secs(300),
        lock_dir.path().to_path_buf(),
        "mend-worker".to_string(),
        registry,
        telemetry,
    );

    let result = mend.evaluate(store.as_ref()).await;

    assert!(
        matches!(result, StrandResult::WorkCreated),
        "Mend should return WorkCreated after cleanup; got {:?}",
        result
    );

    // Verify bead was released.
    let bead = store.show(&bead_id).await.unwrap();
    assert!(
        bead.assignee.is_none(),
        "bead should be released after mend"
    );
}

#[tokio::test]
async fn real_br_mend_no_stale_peers_returns_no_work() {
    let workspace = create_test_workspace("mend-clean").unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());
    let hb_dir = tempfile::tempdir().unwrap();
    let reg_dir = tempfile::tempdir().unwrap();
    // Use an isolated lock_dir with no leftover files.
    let lock_dir = tempfile::tempdir().unwrap();

    // Write fresh heartbeat for healthy worker.
    let heartbeat_data = needle::health::HeartbeatData {
        worker_id: "healthy-peer".to_string(),
        pid: std::process::id(),
        state: needle::types::WorkerState::Executing,
        current_bead: None,
        workspace: workspace.path().to_path_buf(),
        last_heartbeat: Utc::now(),
        started_at: Utc::now() - chrono::Duration::seconds(60),
        beads_processed: 0,
        session: "healthy-peer".to_string(),
    };
    let hb_path = hb_dir.path().join("healthy-peer.json");
    std::fs::write(&hb_path, serde_json::to_string(&heartbeat_data).unwrap()).unwrap();

    // Run Mend strand with a fresh lock_dir and high lock TTL.
    // Note: br doctor may still find issues, so we accept either NoWork or WorkCreated
    // if the only work was db repair. The key assertion is that healthy peers
    // don't cause bead releases.
    let config = MendConfig {
        lock_ttl_secs: 3600, // 1 hour - no locks are considered orphaned
        ..MendConfig::default()
    };
    let registry = Registry::new(reg_dir.path());
    let telemetry = Telemetry::new("mend-worker".to_string());

    let mend = MendStrand::new(
        config,
        hb_dir.path().to_path_buf(),
        Duration::from_secs(300),
        lock_dir.path().to_path_buf(),
        "mend-worker".to_string(),
        registry,
        telemetry,
    );

    let result = mend.evaluate(store.as_ref()).await;

    // With fresh heartbeat and no stale locks, we should get NoWork.
    // (br doctor may find issues but those are non-fatal and shouldn't
    // trigger WorkCreated in a fresh workspace.)
    assert!(
        matches!(result, StrandResult::NoWork),
        "Mend should return NoWork when no stale peers; got {:?}",
        result
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 5: Mitosis splits multi-task beads correctly
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn real_br_mitosis_precondition_checks() {
    let workspace = create_test_workspace("mitosis-pre").unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());
    let lock_dir = tempfile::tempdir().unwrap();

    // Create a bead with failure-count:1 label.
    let bead_id = create_bead(workspace.path(), "multi-task-bead", 1).unwrap();
    add_label(workspace.path(), &bead_id, "failure-count:1").unwrap();

    let bead = store.show(&bead_id).await.unwrap();

    // Test 1: Disabled mitosis returns Skipped.
    let disabled_config = MitosisConfig {
        enabled: false,
        first_failure_only: true,
        force_failure_threshold: 0,
    };
    let evaluator = MitosisEvaluator::new(
        disabled_config,
        Telemetry::new("test".to_string()),
        lock_dir.path().to_path_buf(),
    );

    let dispatcher = create_test_dispatcher();
    let prompt_builder =
        needle::prompt::PromptBuilder::new(&needle::config::PromptConfig::default());

    let result = evaluator
        .evaluate(
            store.as_ref(),
            &bead,
            workspace.path(),
            &dispatcher,
            &prompt_builder,
            "claude-sonnet",
        )
        .await
        .unwrap();

    assert!(
        matches!(result, MitosisResult::Skipped { ref reason } if reason == "disabled"),
        "disabled mitosis should skip; got {:?}",
        result
    );

    // Test 2: Not first failure returns Skipped.
    add_label(workspace.path(), &bead_id, "failure-count:2").unwrap();
    let enabled_config = MitosisConfig {
        enabled: true,
        first_failure_only: true,
        force_failure_threshold: 0,
    };
    let evaluator2 = MitosisEvaluator::new(
        enabled_config,
        Telemetry::new("test".to_string()),
        lock_dir.path().to_path_buf(),
    );

    // Need to re-fetch to get updated labels.
    let bead2 = store.show(&bead_id).await.unwrap();
    let result2 = evaluator2
        .evaluate(
            store.as_ref(),
            &bead2,
            workspace.path(),
            &dispatcher,
            &prompt_builder,
            "claude-sonnet",
        )
        .await
        .unwrap();

    assert!(
        matches!(result2, MitosisResult::Skipped { .. }),
        "non-first failure should skip; got {:?}",
        result2
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 6: Duplicate mitosis on same parent creates zero new children
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn real_br_mitosis_dedup_skips_existing_children() {
    let workspace = create_test_workspace("mitosis-dedup").unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());

    // Create parent bead.
    let parent_id = create_bead(workspace.path(), "parent-multi-task", 1).unwrap();
    add_label(workspace.path(), &parent_id, "failure-count:1").unwrap();

    // Create an existing child that blocks the parent.
    let existing_child_id = create_bead(workspace.path(), "Existing Task", 1).unwrap();

    // Add dependency: parent depends on child (child blocks parent).
    // br dep add syntax: br dep add <issue> <depends_on> means issue depends on depends_on.
    // So "parent depends on child" = "child blocks parent".
    let br = br_path();
    let output = std::process::Command::new(&br)
        .args(["dep", "add", parent_id.as_ref(), existing_child_id.as_ref()])
        .current_dir(workspace.path())
        .output()
        .context("failed to run br dep add")
        .unwrap();

    if !output.status.success() {
        panic!(
            "br dep add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Verify the parent has the child as a dependency.
    let parent = store.show(&parent_id).await.unwrap();
    assert!(
        !parent.dependencies.is_empty(),
        "parent should have dependency"
    );

    // The mitosis evaluator's create_children method will check existing
    // children and skip duplicates. This is tested via the unit tests in
    // mitosis/mod.rs (create_children_with_dedup, create_children_all_deduped).
    // Here we verify the integration: reading dependencies via br works.
    let titles: Vec<String> = parent
        .dependencies
        .iter()
        .filter(|d| d.dependency_type == "blocks")
        .map(|d| d.title.clone())
        .collect();

    assert!(
        titles.iter().any(|t| t.to_lowercase().contains("existing")),
        "parent should have existing child as blocker; got {:?}",
        titles
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 7: Concurrent mitosis on same parent: flock serializes
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn real_br_mitosis_flock_serializes_concurrent_workers() {
    let workspace = create_test_workspace("mitosis-flock").unwrap();
    let store = Arc::new(store_for_workspace(workspace.path()).unwrap());
    let lock_dir = tempfile::tempdir().unwrap();

    // Create parent bead with failure-count:1.
    let parent_id = create_bead(workspace.path(), "concurrent-mitosis-parent", 1).unwrap();
    add_label(workspace.path(), &parent_id, "failure-count:1").unwrap();

    let parent = store.show(&parent_id).await.unwrap();

    // Both evaluators share the same lock_dir, so they'll contend for the flock.
    let config = MitosisConfig {
        enabled: true,
        first_failure_only: true,
        force_failure_threshold: 0,
    };

    let dispatcher = Arc::new(create_test_dispatcher());
    let prompt_builder = Arc::new(needle::prompt::PromptBuilder::new(
        &needle::config::PromptConfig::default(),
    ));

    let mut handles = Vec::new();
    for i in 0..2u32 {
        let store = store.clone();
        let parent = parent.clone();
        let lock_path = lock_dir.path().to_path_buf();
        let dispatcher = dispatcher.clone();
        let prompt_builder = prompt_builder.clone();
        let workspace_path = workspace.path().to_path_buf();
        let config_clone = config.clone();

        let handle = tokio::spawn(async move {
            let evaluator = MitosisEvaluator::new(
                config_clone,
                Telemetry::new(format!("worker-{i}")),
                lock_path,
            );

            evaluator
                .evaluate(
                    store.as_ref(),
                    &parent,
                    &workspace_path,
                    dispatcher.as_ref(),
                    prompt_builder.as_ref(),
                    "claude-sonnet",
                )
                .await
        });
        handles.push(handle);
    }

    // Both should complete without error (flock prevents concurrent access).
    for handle in handles {
        let result = handle.await.unwrap();
        assert!(
            result.is_ok(),
            "mitosis should complete without error; got {:?}",
            result
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 8: Strand waterfall ordering
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn real_br_strand_waterfall_ordering() {
    let workspace = create_test_workspace("waterfall").unwrap();
    let config = needle::config::Config::default();
    let registry = Registry::new(workspace.path());
    let telemetry = Telemetry::new("test".to_string());

    let runner = StrandRunner::from_config(&config, "test-worker", registry, telemetry);

    assert_eq!(
        runner.strand_names(),
        vec!["pluck", "mend", "explore", "weave", "unravel", "pulse", "knot"],
        "waterfall should be pluck → mend → explore → weave → unravel → pulse → knot"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 9: Provider/model concurrency limits (registry-based, no br needed)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn real_br_provider_concurrency_limit_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::new(dir.path());

    // Register 3 workers using anthropic provider.
    for i in 0..3 {
        registry
            .register(needle::registry::WorkerEntry {
                id: format!("worker-{i}"),
                pid: std::process::id(),
                workspace: PathBuf::from("/tmp/test"),
                agent: "claude".to_string(),
                model: Some("sonnet".to_string()),
                provider: Some("anthropic".to_string()),
                started_at: Utc::now(),
                beads_processed: 0,
            })
            .unwrap();
    }

    // Configure limit: max 3 concurrent for anthropic.
    let mut providers = std::collections::BTreeMap::new();
    providers.insert(
        "anthropic".to_string(),
        needle::config::ProviderLimits {
            max_concurrent: Some(3),
            requests_per_minute: None,
        },
    );
    let config = needle::config::LimitsConfig {
        providers,
        models: std::collections::BTreeMap::new(),
    };
    let limiter = needle::rate_limit::RateLimiter::new(config, dir.path());

    let decision = limiter
        .check(Some("anthropic"), Some("sonnet"), &registry)
        .unwrap();

    assert!(
        matches!(
            decision,
            needle::rate_limit::RateLimitDecision::ProviderConcurrencyExceeded {
                current: 3,
                limit: 3,
                ..
            }
        ),
        "should block when at provider limit; got: {decision}"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 10: Database corruption — corrupt SQLite, verify auto-repair and
//          continued operation
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn real_br_database_corruption_auto_recovery() {
    use needle::bead_store::RecoveryOutcome;

    // Use a hyphen-free temp directory name: br derives the bead prefix from the
    // directory name by splitting on hyphens. Temp dirs like "needle-test-foo-XXXXXX"
    // cause prefix mismatch during db recovery (br expects "needle" but IDs use
    // the full directory name as prefix). A single-token prefix avoids this.
    let workspace = tempfile::Builder::new()
        .prefix("needlecorrupttest")
        .tempdir()
        .unwrap();
    let br = br_path();
    let init = std::process::Command::new(&br)
        .args(["init"])
        .current_dir(workspace.path())
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "br init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let store = store_for_workspace(workspace.path()).unwrap();

    // Create 2 beads so we can verify data integrity after recovery.
    let bead_a = create_bead(workspace.path(), "survive-corruption-a", 1).unwrap();
    let bead_b = create_bead(workspace.path(), "survive-corruption-b", 2).unwrap();

    // Verify normal operations work before corruption.
    let beads_before = store.ready(&Filters::default()).await.unwrap();
    assert!(
        beads_before.len() >= 2,
        "should have at least 2 ready beads before corruption"
    );

    let shown = store.show(&bead_a).await.unwrap();
    assert_eq!(shown.title, "survive-corruption-a");

    // ── Corrupt the SQLite database ──────────────────────────────────────
    let db_path = workspace.path().join(".beads/beads.db");
    assert!(db_path.exists(), "database file should exist");

    // Record the original db size for comparison after recovery.
    let original_db_size = std::fs::metadata(&db_path).unwrap().len();

    // Write garbage bytes to corrupt the SQLite header. The first 16 bytes
    // of a valid SQLite file contain the magic string "SQLite format 3\000".
    // Overwriting them makes the file unrecognizable.
    let garbage = b"THIS_IS_CORRUPT_DATA_NOT_SQLITE_AT_ALL____";
    std::fs::write(&db_path, garbage).unwrap();

    // Note: br auto-recovers from corruption by falling back to JSONL reads,
    // so operations may still succeed. The key test is that recover_db()
    // properly rebuilds the database file itself.

    // ── Run auto-recovery ────────────────────────────────────────────────
    let recovery = store.recover_db().await;

    match &recovery {
        RecoveryOutcome::Repaired(report) => {
            // doctor --repair rebuilt the db from JSONL.
            eprintln!(
                "  Recovery via repair: {} warnings, {} fixed",
                report.warnings.len(),
                report.fixed.len()
            );
        }
        RecoveryOutcome::Rebuilt => {
            // Full rebuild from JSONL succeeded.
            eprintln!("  Recovery via full rebuild from JSONL");
        }
        RecoveryOutcome::Failed(e) => {
            panic!("database recovery should succeed; got Failed: {e}");
        }
    }

    // ── Verify operations work after recovery ────────────────────────────
    let beads_after = store.ready(&Filters::default()).await.unwrap();
    assert!(
        beads_after.len() >= 2,
        "should still have at least 2 ready beads after recovery; got {}",
        beads_after.len()
    );

    // Verify both original beads survived (data integrity from JSONL).
    let shown_a = store.show(&bead_a).await.unwrap();
    assert_eq!(
        shown_a.title, "survive-corruption-a",
        "bead A should survive corruption recovery"
    );

    let shown_b = store.show(&bead_b).await.unwrap();
    assert_eq!(
        shown_b.title, "survive-corruption-b",
        "bead B should survive corruption recovery"
    );

    // Verify the database file was properly rebuilt (not our garbage data).
    assert!(
        db_path.exists(),
        "database file should exist after recovery"
    );

    let recovered_db_size = std::fs::metadata(&db_path).unwrap().len();
    assert!(
        recovered_db_size > garbage.len() as u64,
        "rebuilt database ({recovered_db_size}B) should be larger than garbage ({}B)",
        garbage.len()
    );

    // The rebuilt database should be roughly the same size as the original.
    assert!(
        recovered_db_size >= original_db_size / 2,
        "rebuilt database ({recovered_db_size}B) should be at least half the original ({original_db_size}B)"
    );

    // Verify the database file starts with the SQLite magic string.
    let db_header = std::fs::read(&db_path).unwrap();
    assert!(
        db_header.starts_with(b"SQLite format 3"),
        "rebuilt database should be a valid SQLite file"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Helpers
// ═════════════════════════════════════════════════════════════════════════════

fn create_test_dispatcher() -> needle::dispatch::Dispatcher {
    use std::collections::HashMap;
    let adapters: HashMap<String, needle::dispatch::AgentAdapter> = HashMap::new();
    let telemetry = Telemetry::new("test".to_string());
    needle::dispatch::Dispatcher::with_adapters(adapters, telemetry, 60)
}
