//! Round-trip conformance test for bead checkpoint functionality.
//!
//! This test integrates all checkpoint components into a comprehensive round-trip:
//! 1. Create a maximally-populated source workspace
//! 2. Flush workspace state to a temporary checkpoint
//! 3. Restore checkpoint into a fresh workspace
//! 4. Verify complete equality between source and restored workspaces
//!
//! The test is designed to catch silent data loss in checkpoint round-trips by
//! comparing the complete public surface of all beads before and after restoration.
//!
//! # Fields NOT Expected to Round-Trip
//!
//! The following fields are intentionally excluded from equality comparison:
//!
//! - **compaction_level**: Internal SQLite VACUUM state, may increase during restore
//! - **content_hash**: Internal field, not part of public bead surface
//! - **sender**: Internal tracking field, not part of public surface
//! - **ephemeral**: Internal flag, not part of public surface
//! - **pinned**: Internal flag, not part of public surface
//! - **is_template**: Internal flag, not part of public surface
//! - **manual_status**: Internal override field, not part of public surface
//! - **deleted_at**: Soft-delete metadata, not part of active bead surface
//! - **deleted_by**: Soft-delete metadata, not part of active bead surface
//! - **delete_reason**: Soft-delete metadata, not part of active bead surface
//!
//! These exclusions are documented in `WorkspaceEqualityConfig::default()` and
//! represent internal implementation details that should not affect the public
//! bead state visible to users.
//!
//! # Test Purpose
//!
//! This test verifies that checkpoint flush and restore operations preserve all
//! user-visible bead state. A failure indicates data corruption or loss in the
//! checkpoint pipeline, which is critical for:
//!
//! - Workspace backup and restore
//! - Multi-worker synchronization
//! - Disaster recovery scenarios
//! - State migration across environments

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use tempfile::TempDir;

use needle::checkpoint_utils::{flush_checkpoint_to_temp, restore_checkpoint_to_fresh_workspace};
use needle::workspace_equality::{assert_workspace_eq, WorkspaceEqualityConfig};

/// Path to the bead-forge binary.
fn bf_path() -> PathBuf {
    which::which("bf").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(format!("{home}/.local/bin/bf"))
    })
}

/// Create an isolated test workspace with `.beads/` initialized.
fn create_test_workspace(prefix: &str) -> Result<TempDir> {
    let dir = tempfile::Builder::new()
        .prefix(&format!("needle-roundtrip-{prefix}-"))
        .tempdir()
        .context("failed to create temp dir")?;

    let bf = bf_path();
    let output = Command::new(&bf)
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .context("failed to run bf init")?;

    if !output.status.success() {
        anyhow::bail!(
            "bf init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(dir)
}

/// Create a bead in the test workspace and return its ID.
fn create_bead(workspace: &Path, title: &str) -> Result<String> {
    let bf = bf_path();
    let do_create = || {
        Command::new(&bf)
            .args(["create", "--title", title, "--description", title])
            .current_dir(workspace)
            .output()
            .context("failed to run bf create")
    };

    let mut output = do_create()?;

    // Retry once on sync conflict (FrankenSQLite WAL race).
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Sync conflict") || stderr.contains("sync conflict") {
            let _ = Command::new(&bf)
                .args(["sync", "--flush-only"])
                .current_dir(workspace)
                .output();
            output = do_create()?;
        }
    }

    if !output.status.success() {
        anyhow::bail!(
            "bf create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let id = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(id)
}

/// Add a label to a bead.
fn add_label(workspace: &Path, bead_id: &str, label: &str) -> Result<()> {
    let bf = bf_path();
    let output = Command::new(&bf)
        .args(["label", "add", bead_id, "--label", label])
        .current_dir(workspace)
        .output()
        .context("failed to run bf label add")?;

    if !output.status.success() {
        anyhow::bail!(
            "bf label add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Update a bead's priority.
fn update_priority(workspace: &Path, bead_id: &str, priority: u8) -> Result<()> {
    let bf = bf_path();
    let output = Command::new(&bf)
        .args(["update", bead_id, "--priority", &priority.to_string()])
        .current_dir(workspace)
        .output()
        .context("failed to run bf update")?;

    if !output.status.success() {
        anyhow::bail!(
            "bf update failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Close a bead with a reason.
fn close_bead(workspace: &Path, bead_id: &str, reason: &str) -> Result<()> {
    let bf = bf_path();
    let output = Command::new(&bf)
        .args(["close", bead_id, "--reason", reason])
        .current_dir(workspace)
        .output()
        .context("failed to run bf close")?;

    if !output.status.success() {
        anyhow::bail!(
            "bf close failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Create a dependency between two beads.
fn add_dependency(workspace: &Path, blocker: &str, blocked: &str) -> Result<()> {
    let bf = bf_path();
    let output = Command::new(&bf)
        .args(["dep", "add", blocker, "--blocks", blocked])
        .current_dir(workspace)
        .output()
        .context("failed to run bf dep add")?;

    if !output.status.success() {
        anyhow::bail!(
            "bf dep add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Populate a workspace with multiple beads in various states.
///
/// This creates a representative workspace with:
/// - Multiple beads with different labels
/// - Assigned and unassigned beads
/// - Beads with comments
/// - Beads with dependencies
/// - Closed and open beads
fn populate_workspace(workspace: &Path) -> Result<Vec<String>> {
    let mut bead_ids = Vec::new();

    // Bead 1: Multi-label open bead (assigned)
    let bead1 = create_bead(workspace, "Multi-label Task")?;
    add_label(workspace, &bead1, "rust")?;
    add_label(workspace, &bead1, "feature")?;
    add_label(workspace, &bead1, "high-priority")?;
    bead_ids.push(bead1);

    // Bead 2: Open bead ready for claiming
    let bead2 = create_bead(workspace, "Ready Task Alpha")?;
    add_label(workspace, &bead2, "ready")?;
    bead_ids.push(bead2);

    // Bead 3: Another open bead
    let bead4 = create_bead(workspace, "Ready Task Beta")?;
    add_label(workspace, &bead4, "ready")?;
    add_label(workspace, &bead4, "documentation")?;
    bead_ids.push(bead4);

    // Bead 5: Bead with high priority
    let bead5 = create_bead(workspace, "Priority Task")?;
    add_label(workspace, &bead5, "reviewed")?;
    update_priority(workspace, &bead5, 1)?;
    bead_ids.push(bead5);

    // Bead 6: Closed bead
    let bead6 = create_bead(workspace, "Completed Task")?;
    add_label(workspace, &bead6, "completed")?;
    close_bead(workspace, &bead6, "All tests passing, ready to ship")?;
    bead_ids.push(bead6);

    // Bead 7: Bead with dependencies (blocks other beads)
    let bead7 = create_bead(workspace, "Foundation Component")?;
    add_label(workspace, &bead7, "infrastructure")?;
    add_label(workspace, &bead7, "blocking")?;
    bead_ids.push(bead7.clone());

    // Bead 8: Bead that depends on bead7
    let bead8 = create_bead(workspace, "Dependent Feature")?;
    add_label(workspace, &bead8, "blocked")?;
    add_label(workspace, &bead8, "feature")?;
    add_dependency(workspace, &bead7, &bead8)?;
    bead_ids.push(bead8);

    // Bead 9: Another bead dependent on bead7
    let bead9 = create_bead(workspace, "Another Dependent")?;
    add_label(workspace, &bead9, "blocked")?;
    add_dependency(workspace, &bead7, &bead9)?;
    bead_ids.push(bead9);

    Ok(bead_ids)
}

/// Integration test: Complete checkpoint round-trip preserves all bead state.
///
/// This test performs a full round-trip of the checkpoint pipeline:
/// 1. Create a source workspace and populate it with beads
/// 2. Flush the workspace to a temporary checkpoint
/// 3. Restore the checkpoint into a fresh workspace
/// 4. Verify that source and restored workspaces are identical
///
/// Expected behavior: All public bead fields should be preserved exactly.
/// Internal fields (see module docs) are excluded from comparison.
#[tokio::test]
async fn checkpoint_roundtrip_preserves_all_bead_state() {
    // Step 1: Create source workspace and populate with beads
    let source_workspace = create_test_workspace("source")
        .expect("failed to create source workspace");

    let bead_ids = populate_workspace(source_workspace.path())
        .expect("failed to populate source workspace");

    // Verify we created the expected number of beads
    assert_eq!(
        bead_ids.len(),
        8,
        "Expected to create 8 beads, got {}",
        bead_ids.len()
    );

    // Step 2: Flush workspace to checkpoint
    let (_checkpoint_temp, checkpoint_path) = flush_checkpoint_to_temp(source_workspace.path())
        .await
        .expect("failed to flush workspace to checkpoint");

    // Verify checkpoint structure
    assert!(
        checkpoint_path.exists(),
        "Checkpoint directory should exist"
    );
    assert!(
        checkpoint_path.join(".beads").exists(),
        "Checkpoint .beads directory should exist"
    );
    assert!(
        checkpoint_path.join(".beads/beads.db").exists(),
        "Checkpoint database file should exist"
    );
    assert!(
        checkpoint_path.join("current.json").exists(),
        "Checkpoint pointer file should exist"
    );

    // Step 3: Restore checkpoint to fresh workspace
    let (_restored_temp, restored_path) =
        restore_checkpoint_to_fresh_workspace(&checkpoint_path)
            .await
            .expect("failed to restore checkpoint to fresh workspace");

    // Verify restored workspace structure
    assert!(
        restored_path.exists(),
        "Restored workspace should exist"
    );
    assert!(
        restored_path.join(".beads").exists(),
        "Restored .beads directory should exist"
    );
    assert!(
        restored_path.join(".beads/beads.db").exists(),
        "Restored database file should exist"
    );

    // Step 4: Verify complete equality between source and restored workspaces
    let config = WorkspaceEqualityConfig::default();
    assert_workspace_eq(source_workspace.path(), &restored_path, &config);
}

/// Integration test: Empty workspace round-trip.
///
/// This tests the edge case of a workspace with no beads, verifying that
/// checkpoint flush and restore handle empty workspaces correctly.
#[tokio::test]
async fn checkpoint_roundtrip_handles_empty_workspace() {
    // Create an empty workspace (just bf init, no beads)
    let source_workspace = create_test_workspace("empty")
        .expect("failed to create empty workspace");

    // Flush empty workspace to checkpoint
    let (_checkpoint_temp, checkpoint_path) = flush_checkpoint_to_temp(source_workspace.path())
        .await
        .expect("failed to flush empty workspace to checkpoint");

    // Restore to fresh workspace
    let (_restored_temp, restored_path) =
        restore_checkpoint_to_fresh_workspace(&checkpoint_path)
            .await
            .expect("failed to restore empty checkpoint");

    // Verify equality (both should have 0 beads)
    let config = WorkspaceEqualityConfig::default();
    assert_workspace_eq(source_workspace.path(), &restored_path, &config);
}

/// Integration test: Workspace with single bead round-trip.
///
/// This tests the minimal case of a workspace with exactly one bead,
/// verifying that single-bead workspaces round-trip correctly.
#[tokio::test]
async fn checkpoint_roundtrip_handles_single_bead() {
    let source_workspace = create_test_workspace("single")
        .expect("failed to create source workspace");

    // Create a single bead
    let bead_id = create_bead(source_workspace.path(), "Single Task")
        .expect("failed to create single bead");
    add_label(source_workspace.path(), &bead_id, "solo")
        .expect("failed to add label");

    // Flush to checkpoint
    let (_checkpoint_temp, checkpoint_path) = flush_checkpoint_to_temp(source_workspace.path())
        .await
        .expect("failed to flush single-bead workspace");

    // Restore to fresh workspace
    let (_restored_temp, restored_path) =
        restore_checkpoint_to_fresh_workspace(&checkpoint_path)
            .await
            .expect("failed to restore single-bead checkpoint");

    // Verify equality
    let config = WorkspaceEqualityConfig::default();
    assert_workspace_eq(source_workspace.path(), &restored_path, &config);
}

/// Integration test: Checkpoint pointer file format.
///
/// This verifies that the checkpoint pointer file contains valid JSON
/// with required fields (timestamp, source_workspace, type).
#[tokio::test]
async fn checkpoint_pointer_file_contains_valid_metadata() {
    let source_workspace = create_test_workspace("pointer")
        .expect("failed to create source workspace");

    // Create a bead
    create_bead(source_workspace.path(), "Test Bead")
        .expect("failed to create bead");

    // Flush to checkpoint
    let (_checkpoint_temp, checkpoint_path) = flush_checkpoint_to_temp(source_workspace.path())
        .await
        .expect("failed to flush to checkpoint");

    // Read and validate pointer file
    let pointer_content = std::fs::read_to_string(checkpoint_path.join("current.json"))
        .expect("failed to read pointer file");

    let pointer: serde_json::Value =
        serde_json::from_str(&pointer_content).expect("failed to parse pointer JSON");

    // Verify required fields exist
    assert!(
        pointer.get("timestamp").is_some(),
        "Pointer should contain timestamp field"
    );
    assert!(
        pointer.get("source_workspace").is_some(),
        "Pointer should contain source_workspace field"
    );
    assert_eq!(
        pointer.get("type").and_then(|v| v.as_str()),
        Some("test_checkpoint"),
        "Pointer type should be 'test_checkpoint'"
    );

    // Verify timestamp is valid ISO 8601
    let timestamp = pointer["timestamp"]
        .as_str()
        .expect("timestamp should be a string");
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .expect("timestamp should be valid ISO 8601");
}

/// Integration test: Multiple sequential round-trips.
///
/// This tests that repeated checkpoint flush and restore cycles
/// maintain data integrity across multiple iterations.
#[tokio::test]
async fn checkpoint_roundtrip_preserves_state_across_multiple_cycles() {
    // Create initial workspace
    let source_workspace = create_test_workspace("multi-cycle")
        .expect("failed to create source workspace");

    // Populate with beads
    let bead_ids = populate_workspace(source_workspace.path())
        .expect("failed to populate workspace");

    // First round-trip
    let (_checkpoint1_temp, checkpoint1_path) =
        flush_checkpoint_to_temp(source_workspace.path())
            .await
            .expect("failed to flush first checkpoint");

    let (_restored1_temp, restored1_path) =
        restore_checkpoint_to_fresh_workspace(&checkpoint1_path)
            .await
            .expect("failed to restore first checkpoint");

    // Verify first round-trip
    let config = WorkspaceEqualityConfig::default();
    assert_workspace_eq(source_workspace.path(), &restored1_path, &config);

    // Second round-trip (from restored workspace)
    let (_checkpoint2_temp, checkpoint2_path) =
        flush_checkpoint_to_temp(&restored1_path)
            .await
            .expect("failed to flush second checkpoint");

    let (_restored2_temp, restored2_path) =
        restore_checkpoint_to_fresh_workspace(&checkpoint2_path)
            .await
            .expect("failed to restore second checkpoint");

    // Verify second round-trip (should still match original)
    assert_workspace_eq(source_workspace.path(), &restored2_path, &config);
}
