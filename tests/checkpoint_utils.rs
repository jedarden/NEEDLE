//! Test utilities for checkpoint flush and restore operations.
//!
//! This module provides helper functions for testing bead-forge checkpoint
//! functionality by flushing workspace state to temporary directories and
//! restoring it into fresh empty workspaces.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Flush a workspace checkpoint to a temporary directory.
///
/// This function creates a temporary directory, flushes the current workspace
/// state using bead-forge's checkpoint mechanism, and returns the path to the
/// checkpoint directory.
///
/// # Arguments
///
/// * `workspace` - Path to the workspace to flush
///
/// # Returns
///
/// A tuple containing:
/// * The temporary directory handle (keeps the directory alive while in scope)
/// * The path to the checkpoint directory within the temp dir
///
/// # Example
///
/// ```no_run
/// use needle::checkpoint_utils::flush_checkpoint_to_temp;
///
/// # async fn example() -> anyhow::Result<()> {
/// let workspace = PathBuf::from("/path/to/workspace");
/// let (_temp_dir, checkpoint_path) = flush_checkpoint_to_temp(&workspace).await?;
/// println!("Checkpoint flushed to: {:?}", checkpoint_path);
/// # Ok(())
/// # }
/// ```
pub async fn flush_checkpoint_to_temp(workspace: &Path) -> Result<(TempDir, PathBuf)> {
    // Create a temporary directory for the checkpoint
    let temp_dir = TempDir::new().context("failed to create temporary directory for checkpoint")?;

    // Create checkpoint directory structure
    let checkpoint_dir = temp_dir.path().join("checkpoint");
    fs::create_dir_all(&checkpoint_dir).with_context(|| {
        format!(
            "failed to create checkpoint directory at {:?}",
            checkpoint_dir
        )
    })?;

    // Copy the .beads directory to the checkpoint location
    let source_beads = workspace.join(".beads");
    if !source_beads.exists() {
        anyhow::bail!("workspace .beads directory not found at {:?}", source_beads);
    }

    let checkpoint_beads = checkpoint_dir.join(".beads");
    copy_dir_recursive(&source_beads, &checkpoint_beads).with_context(|| {
        format!(
            "failed to copy .beads to checkpoint at {:?}",
            checkpoint_beads
        )
    })?;

    // Create a pointer file indicating this is a checkpoint
    let pointer_file = checkpoint_dir.join("current.json");
    let pointer_content = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "source_workspace": workspace,
        "type": "test_checkpoint"
    });
    fs::write(&pointer_file, pointer_content.to_string())
        .with_context(|| format!("failed to write checkpoint pointer at {:?}", pointer_file))?;

    tracing::debug!(
        workspace = %workspace.display(),
        checkpoint = %checkpoint_dir.display(),
        "flushed workspace checkpoint to temporary directory"
    );

    Ok((temp_dir, checkpoint_dir))
}

/// Restore a checkpoint into a fresh empty workspace.
///
/// This function creates a new empty workspace at the target path and restores
/// the checkpoint from the given path into it.
///
/// # Arguments
///
/// * `target_workspace` - Path where the fresh workspace should be created
/// * `checkpoint_path` - Path to the checkpoint directory to restore from
///
/// # Returns
///
/// `Ok(())` if the restore succeeded
///
/// # Errors
///
/// Returns an error if:
/// - The target workspace already exists
/// - The checkpoint directory is invalid
/// - The restore operation fails
///
/// # Example
///
/// ```no_run
/// use needle::checkpoint_utils::restore_checkpoint_from_path;
///
/// # async fn example() -> anyhow::Result<()> {
/// let target = PathBuf::from("/tmp/test_workspace");
/// let checkpoint = PathBuf::from("/tmp/checkpoint");
/// restore_checkpoint_from_path(&target, &checkpoint).await?;
/// println!("Checkpoint restored to: {:?}", target);
/// # Ok(())
/// # }
/// ```
pub async fn restore_checkpoint_from_path(
    target_workspace: &Path,
    checkpoint_path: &Path,
) -> Result<()> {
    // Verify the checkpoint exists and is valid
    let checkpoint_beads = checkpoint_path.join(".beads");
    if !checkpoint_beads.exists() {
        anyhow::bail!(
            "checkpoint .beads directory not found at {:?}",
            checkpoint_beads
        );
    }

    // Verify the checkpoint pointer file exists
    let pointer_file = checkpoint_path.join("current.json");
    if !pointer_file.exists() {
        anyhow::bail!("checkpoint pointer file not found at {:?}", pointer_file);
    }

    // Create target workspace directory
    if target_workspace.exists() {
        anyhow::bail!("target workspace already exists at {:?}", target_workspace);
    }
    fs::create_dir_all(target_workspace).with_context(|| {
        format!(
            "failed to create target workspace at {:?}",
            target_workspace
        )
    })?;

    // Copy checkpoint beads to target workspace
    let target_beads = target_workspace.join(".beads");
    copy_dir_recursive(&checkpoint_beads, &target_beads)
        .with_context(|| format!("failed to restore checkpoint to {:?}", target_beads))?;

    tracing::debug!(
        target = %target_workspace.display(),
        checkpoint = %checkpoint_path.display(),
        "restored checkpoint into fresh workspace"
    );

    Ok(())
}

/// Restore a checkpoint into a fresh empty workspace (auto-created).
///
/// This function creates a new temporary workspace directory and restores
/// the checkpoint from the given path into it. The workspace is created
/// automatically and returned along with its path.
///
/// # Arguments
///
/// * `checkpoint_path` - Path to the checkpoint directory to restore from
///
/// # Returns
///
/// A tuple containing:
/// * The temporary directory handle (keeps the workspace alive while in scope)
/// * The path to the restored workspace
///
/// # Errors
///
/// Returns an error if:
/// - The checkpoint directory is invalid
/// - The workspace creation fails
/// - The restore operation fails
///
/// # Example
///
/// ```no_run
/// use needle::checkpoint_utils::restore_checkpoint_to_fresh_workspace;
///
/// # async fn example() -> anyhow::Result<()> {
/// let checkpoint = PathBuf::from("/tmp/checkpoint");
/// let (_temp_dir, workspace_path) = restore_checkpoint_to_fresh_workspace(&checkpoint).await?;
/// println!("Checkpoint restored to fresh workspace: {:?}", workspace_path);
/// # Ok(())
/// # }
/// ```
pub async fn restore_checkpoint_to_fresh_workspace(
    checkpoint_path: &Path,
) -> Result<(TempDir, PathBuf)> {
    // Verify the checkpoint exists and is valid before creating workspace
    let checkpoint_beads = checkpoint_path.join(".beads");
    if !checkpoint_beads.exists() {
        anyhow::bail!(
            "checkpoint .beads directory not found at {:?}",
            checkpoint_beads
        );
    }

    // Verify the checkpoint pointer file exists
    let pointer_file = checkpoint_path.join("current.json");
    if !pointer_file.exists() {
        anyhow::bail!("checkpoint pointer file not found at {:?}", pointer_file);
    }

    // Create a fresh empty workspace directory
    let temp_workspace = TempDir::new().context("failed to create temporary workspace directory")?;
    let workspace_path = temp_workspace.path().to_path_buf();

    tracing::debug!(
        workspace = %workspace_path.display(),
        checkpoint = %checkpoint_path.display(),
        "created fresh workspace for checkpoint restoration"
    );

    // Copy checkpoint beads to the new workspace
    let workspace_beads = workspace_path.join(".beads");
    copy_dir_recursive(&checkpoint_beads, &workspace_beads)
        .with_context(|| format!("failed to restore checkpoint to {:?}", workspace_beads))?;

    // Validate that the restored workspace has the expected state
    if !workspace_beads.exists() {
        anyhow::bail!(
            "restored workspace .beads directory not found at {:?}",
            workspace_beads
        );
    }

    // Verify at least the database file exists
    let db_file = workspace_beads.join("beads.db");
    if !db_file.exists() {
        anyhow::bail!(
            "restored workspace database file not found at {:?}",
            db_file
        );
    }

    tracing::debug!(
        workspace = %workspace_path.display(),
        "successfully restored checkpoint into fresh workspace"
    );

    Ok((temp_workspace, workspace_path))
}

/// Helper function to recursively copy a directory.
///
/// This is a simple implementation for test use. For production use,
/// consider using a more robust library like `fs_extra` or `walkdir`.
fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<()> {
    if !source.exists() {
        anyhow::bail!("source directory does not exist: {:?}", source);
    }

    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create directory at {:?}", destination))?;

    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read directory at {:?}", source))?
    {
        let entry = entry?;
        let src_path = entry.path();
        let dest_path = destination.join(entry.file_name());

        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&src_path, &dest_path)?;
        } else {
            fs::copy(&src_path, &dest_path)
                .with_context(|| format!("failed to copy {:?} to {:?}", src_path, dest_path))?;
        }
    }

    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────────
// Unit Tests
// ────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[tokio::test]
    async fn flush_checkpoint_to_temp_creates_valid_checkpoint() {
        // Create a mock workspace with .beads directory
        let workspace = TempDir::new().expect("failed to create temp workspace");
        let workspace_path = workspace.path();
        let beads_dir = workspace_path.join(".beads");
        fs::create_dir_all(&beads_dir).expect("failed to create .beads directory");

        // Create a simple bead database file
        let db_file = beads_dir.join("beads.db");
        fs::write(&db_file, b"mock bead database").expect("failed to write database file");

        // Flush checkpoint
        let (temp_dir, checkpoint_path) = flush_checkpoint_to_temp(workspace_path)
            .await
            .expect("flush failed");

        // Verify checkpoint structure
        assert!(
            checkpoint_path.exists(),
            "checkpoint directory should exist"
        );
        assert!(
            checkpoint_path.join(".beads").exists(),
            "checkpoint .beads directory should exist"
        );
        assert!(
            checkpoint_path.join(".beads/beads.db").exists(),
            "checkpoint database file should exist"
        );
        assert!(
            checkpoint_path.join("current.json").exists(),
            "checkpoint pointer file should exist"
        );

        // Verify temp directory is still alive
        assert!(
            temp_dir.path().exists(),
            "temp directory should still exist"
        );

        // Verify pointer file content
        let pointer_content = fs::read_to_string(checkpoint_path.join("current.json"))
            .expect("failed to read pointer file");
        let pointer: serde_json::Value =
            serde_json::from_str(&pointer_content).expect("failed to parse pointer file");
        assert_eq!(pointer["type"], "test_checkpoint");
        assert!(pointer.get("timestamp").is_some());
        assert_eq!(
            pointer["source_workspace"]
                .as_str()
                .map(|p| PathBuf::from(p)),
            Some(workspace_path.to_path_buf())
        );
    }

    #[tokio::test]
    async fn flush_checkpoint_to_temp_fails_without_beads_dir() {
        // Create a workspace without .beads directory
        let workspace = TempDir::new().expect("failed to create temp workspace");
        let workspace_path = workspace.path();

        // Flush should fail
        let result = flush_checkpoint_to_temp(workspace_path).await;
        assert!(
            result.is_err(),
            "flush should fail without .beads directory"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("workspace .beads directory not found"),
            "error should mention missing .beads directory"
        );
    }

    #[tokio::test]
    async fn restore_checkpoint_from_path_creates_fresh_workspace() {
        // Create a checkpoint with .beads directory
        let checkpoint = TempDir::new().expect("failed to create checkpoint temp dir");
        let checkpoint_path = checkpoint.path();
        let checkpoint_beads = checkpoint_path.join(".beads");
        fs::create_dir_all(&checkpoint_beads).expect("failed to create checkpoint .beads");

        // Create a pointer file
        let pointer_file = checkpoint_path.join("current.json");
        fs::write(
            &pointer_file,
            r#"{"type":"test_checkpoint","timestamp":"2026-01-01T00:00:00Z"}"#,
        )
        .expect("failed to write pointer file");

        // Create a mock bead database
        let db_file = checkpoint_beads.join("beads.db");
        fs::write(&db_file, b"mock bead database").expect("failed to write database file");

        // Restore to target workspace
        let target_workspace = TempDir::new().expect("failed to create target temp dir");
        let target_path = target_workspace.path().join("restored_workspace");

        restore_checkpoint_from_path(&target_path, checkpoint_path)
            .await
            .expect("restore failed");

        // Verify target workspace structure
        assert!(target_path.exists(), "target workspace should exist");
        assert!(
            target_path.join(".beads").exists(),
            "target .beads directory should exist"
        );
        assert!(
            target_path.join(".beads/beads.db").exists(),
            "target database file should exist"
        );

        // Verify content was copied correctly
        let original_content = fs::read_to_string(db_file).expect("failed to read original");
        let restored_content = fs::read_to_string(target_path.join(".beads/beads.db"))
            .expect("failed to read restored");
        assert_eq!(original_content, restored_content, "content should match");
    }

    #[tokio::test]
    async fn restore_checkpoint_from_path_fails_with_existing_workspace() {
        // Create a checkpoint
        let checkpoint = TempDir::new().expect("failed to create checkpoint temp dir");
        let checkpoint_path = checkpoint.path();
        let checkpoint_beads = checkpoint_path.join(".beads");
        fs::create_dir_all(&checkpoint_beads).expect("failed to create checkpoint .beads");

        let pointer_file = checkpoint_path.join("current.json");
        fs::write(&pointer_file, r#"{"type":"test_checkpoint"}"#)
            .expect("failed to write pointer file");

        // Create a pre-existing target workspace
        let target_workspace = TempDir::new().expect("failed to create target temp dir");
        let target_path = target_workspace.path().join("existing_workspace");
        fs::create_dir(&target_path).expect("failed to create existing workspace");

        // Restore should fail
        let result = restore_checkpoint_from_path(&target_path, checkpoint_path).await;
        assert!(
            result.is_err(),
            "restore should fail with existing workspace"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("already exists"),
            "error should mention workspace already exists"
        );
    }

    #[tokio::test]
    async fn restore_checkpoint_from_path_fails_with_invalid_checkpoint() {
        // Create an invalid checkpoint (missing .beads directory)
        let checkpoint = TempDir::new().expect("failed to create checkpoint temp dir");
        let checkpoint_path = checkpoint.path();

        // Don't create .beads directory or pointer file

        let target_workspace = TempDir::new().expect("failed to create target temp dir");
        let target_path = target_workspace.path().join("target");

        let result = restore_checkpoint_from_path(&target_path, checkpoint_path).await;
        assert!(
            result.is_err(),
            "restore should fail with invalid checkpoint"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("checkpoint .beads directory not found"),
            "error should mention missing .beads directory"
        );
    }

    #[tokio::test]
    async fn restore_checkpoint_from_path_fails_without_pointer_file() {
        // Create a checkpoint without pointer file
        let checkpoint = TempDir::new().expect("failed to create checkpoint temp dir");
        let checkpoint_path = checkpoint.path();
        let checkpoint_beads = checkpoint_path.join(".beads");
        fs::create_dir_all(&checkpoint_beads).expect("failed to create checkpoint .beads");

        // Don't create pointer file

        let target_workspace = TempDir::new().expect("failed to create target temp dir");
        let target_path = target_workspace.path().join("target");

        let result = restore_checkpoint_from_path(&target_path, checkpoint_path).await;
        assert!(result.is_err(), "restore should fail without pointer file");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("pointer file not found"),
            "error should mention missing pointer file"
        );
    }

    #[tokio::test]
    async fn copy_dir_recursive_handles_nested_directories() {
        // Create a nested directory structure
        let source = TempDir::new().expect("failed to create source temp dir");
        let source_path = source.path();
        let nested_dir = source_path.join("level1/level2/level3");
        fs::create_dir_all(&nested_dir).expect("failed to create nested directories");

        // Create files at each level
        fs::write(source_path.join("file1.txt"), b"root").expect("failed to write root file");
        fs::write(source_path.join("level1/file2.txt"), b"level1")
            .expect("failed to write level1 file");
        fs::write(source_path.join("level1/level2/file3.txt"), b"level2")
            .expect("failed to write level2 file");
        fs::write(
            source_path.join("level1/level2/level3/file4.txt"),
            b"level3",
        )
        .expect("failed to write level3 file");

        // Copy to destination
        let destination = TempDir::new().expect("failed to create destination temp dir");
        let dest_path = destination.path().join("copied");
        copy_dir_recursive(source_path, &dest_path).expect("copy failed");

        // Verify all files and directories were copied
        assert!(dest_path.exists(), "destination root should exist");
        assert!(
            dest_path.join("file1.txt").exists(),
            "root file should exist"
        );
        assert!(
            dest_path.join("level1").exists(),
            "level1 directory should exist"
        );
        assert!(
            dest_path.join("level1/file2.txt").exists(),
            "level1 file should exist"
        );
        assert!(
            dest_path.join("level1/level2").exists(),
            "level2 directory should exist"
        );
        assert!(
            dest_path.join("level1/level2/file3.txt").exists(),
            "level2 file should exist"
        );
        assert!(
            dest_path.join("level1/level2/level3").exists(),
            "level3 directory should exist"
        );
        assert!(
            dest_path.join("level1/level2/level3/file4.txt").exists(),
            "level3 file should exist"
        );

        // Verify file contents
        assert_eq!(
            fs::read_to_string(dest_path.join("file1.txt")).expect("failed to read"),
            "root",
            "root file content should match"
        );
        assert_eq!(
            fs::read_to_string(dest_path.join("level1/level2/level3/file4.txt"))
                .expect("failed to read"),
            "level3",
            "nested file content should match"
        );
    }

    #[tokio::test]
    async fn copy_dir_recursive_fails_with_nonexistent_source() {
        let source = PathBuf::from("/nonexistent/path");
        let destination = TempDir::new().expect("failed to create destination temp dir");
        let dest_path = destination.path().join("target");

        let result = copy_dir_recursive(&source, &dest_path);
        assert!(result.is_err(), "copy should fail with nonexistent source");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("source directory does not exist"),
            "error should mention source doesn't exist"
        );
    }

    #[tokio::test]
    async fn flush_and_restore_roundtrip_preserves_content() {
        // Create a mock workspace with some content
        let workspace = TempDir::new().expect("failed to create workspace temp dir");
        let workspace_path = workspace.path();
        let beads_dir = workspace_path.join(".beads");
        fs::create_dir_all(&beads_dir).expect("failed to create .beads");

        // Create multiple files to test preservation
        let db_file = beads_dir.join("beads.db");
        fs::write(&db_file, b"database content").expect("failed to write db");

        let jsonl_file = beads_dir.join("issues.jsonl");
        fs::write(&jsonl_file, b"line1\nline2\nline3").expect("failed to write jsonl");

        let events_file = beads_dir.join("events.jsonl");
        fs::write(&events_file, b"event1\nevent2").expect("failed to write events");

        // Flush checkpoint
        let (_temp_dir, checkpoint_path) = flush_checkpoint_to_temp(workspace_path)
            .await
            .expect("flush failed");

        // Restore to new workspace
        let target_workspace = TempDir::new().expect("failed to create target temp dir");
        let target_path = target_workspace.path().join("restored");

        restore_checkpoint_from_path(&target_path, &checkpoint_path)
            .await
            .expect("restore failed");

        // Verify all files were preserved
        assert!(
            target_path.join(".beads/beads.db").exists(),
            "database file should be restored"
        );
        assert!(
            target_path.join(".beads/issues.jsonl").exists(),
            "jsonl file should be restored"
        );
        assert!(
            target_path.join(".beads/events.jsonl").exists(),
            "events file should be restored"
        );

        // Verify content matches
        let original_db = fs::read_to_string(db_file).expect("failed to read original db");
        let restored_db = fs::read_to_string(target_path.join(".beads/beads.db"))
            .expect("failed to read restored db");
        assert_eq!(original_db, restored_db, "database content should match");

        let original_jsonl = fs::read_to_string(jsonl_file).expect("failed to read original jsonl");
        let restored_jsonl = fs::read_to_string(target_path.join(".beads/issues.jsonl"))
            .expect("failed to read restored jsonl");
        assert_eq!(original_jsonl, restored_jsonl, "jsonl content should match");
    }

    #[tokio::test]
    async fn restore_checkpoint_to_fresh_workspace_creates_valid_workspace() {
        // Create a checkpoint with .beads directory
        let checkpoint = TempDir::new().expect("failed to create checkpoint temp dir");
        let checkpoint_path = checkpoint.path();
        let checkpoint_beads = checkpoint_path.join(".beads");
        fs::create_dir_all(&checkpoint_beads).expect("failed to create checkpoint .beads");

        // Create a pointer file
        let pointer_file = checkpoint_path.join("current.json");
        fs::write(
            &pointer_file,
            r#"{"type":"test_checkpoint","timestamp":"2026-01-01T00:00:00Z"}"#,
        )
        .expect("failed to write pointer file");

        // Create a mock bead database
        let db_file = checkpoint_beads.join("beads.db");
        fs::write(&db_file, b"mock bead database").expect("failed to write database file");

        // Restore to fresh workspace
        let (temp_workspace, workspace_path) =
            restore_checkpoint_to_fresh_workspace(checkpoint_path)
                .await
                .expect("restore failed");

        // Verify workspace structure
        assert!(workspace_path.exists(), "workspace should exist");
        assert!(
            workspace_path.join(".beads").exists(),
            "workspace .beads directory should exist"
        );
        assert!(
            workspace_path.join(".beads/beads.db").exists(),
            "workspace database file should exist"
        );

        // Verify temp directory is still alive
        assert!(
            temp_workspace.path().exists(),
            "temp directory should still exist"
        );

        // Verify content was copied correctly
        let original_content = fs::read_to_string(db_file).expect("failed to read original");
        let restored_content =
            fs::read_to_string(workspace_path.join(".beads/beads.db"))
                .expect("failed to read restored");
        assert_eq!(original_content, restored_content, "content should match");
    }

    #[tokio::test]
    async fn restore_checkpoint_to_fresh_workspace_fails_with_missing_beads_dir() {
        // Create a checkpoint without .beads directory
        let checkpoint = TempDir::new().expect("failed to create checkpoint temp dir");
        let checkpoint_path = checkpoint.path();

        // Don't create .beads directory - should fail immediately

        let result = restore_checkpoint_to_fresh_workspace(checkpoint_path).await;
        assert!(result.is_err(), "restore should fail without .beads directory");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("checkpoint .beads directory not found"),
            "error should mention missing .beads directory"
        );
    }

    #[tokio::test]
    async fn restore_checkpoint_to_fresh_workspace_fails_with_missing_pointer_file() {
        // Create a checkpoint without pointer file
        let checkpoint = TempDir::new().expect("failed to create checkpoint temp dir");
        let checkpoint_path = checkpoint.path();
        let checkpoint_beads = checkpoint_path.join(".beads");
        fs::create_dir_all(&checkpoint_beads).expect("failed to create checkpoint .beads");

        // Don't create pointer file - should fail
        let result = restore_checkpoint_to_fresh_workspace(checkpoint_path).await;
        assert!(result.is_err(), "restore should fail without pointer file");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("pointer file not found"),
            "error should mention missing pointer file"
        );
    }

    #[tokio::test]
    async fn restore_checkpoint_to_fresh_workspace_preserves_nested_structure() {
        // Create a checkpoint with nested .beads structure
        let checkpoint = TempDir::new().expect("failed to create checkpoint temp dir");
        let checkpoint_path = checkpoint.path();
        let checkpoint_beads = checkpoint_path.join(".beads");
        fs::create_dir_all(&checkpoint_beads).expect("failed to create checkpoint .beads");

        // Create nested directory structure
        let nested_dir = checkpoint_beads.join("nested/data");
        fs::create_dir_all(&nested_dir).expect("failed to create nested dirs");

        // Create multiple files at different levels
        fs::write(checkpoint_beads.join("beads.db"), b"database")
            .expect("failed to write db");
        fs::write(checkpoint_beads.join("issues.jsonl"), b"issues")
            .expect("failed to write issues");
        fs::write(nested_dir.join("config.json"), b"config")
            .expect("failed to write config");

        // Create pointer file
        let pointer_file = checkpoint_path.join("current.json");
        fs::write(&pointer_file, r#"{"type":"test_checkpoint"}"#)
            .expect("failed to write pointer file");

        // Restore to fresh workspace
        let (_temp_workspace, workspace_path) =
            restore_checkpoint_to_fresh_workspace(checkpoint_path)
                .await
                .expect("restore failed");

        // Verify all files and directories were preserved
        assert!(
            workspace_path.join(".beads/beads.db").exists(),
            "database file should be restored"
        );
        assert!(
            workspace_path.join(".beads/issues.jsonl").exists(),
            "jsonl file should be restored"
        );
        assert!(
            workspace_path.join(".beads/nested/data/config.json").exists(),
            "nested file should be restored"
        );

        // Verify file contents match
        let original_db =
            fs::read_to_string(checkpoint_beads.join("beads.db")).expect("failed to read");
        let restored_db =
            fs::read_to_string(workspace_path.join(".beads/beads.db")).expect("failed to read");
        assert_eq!(original_db, restored_db, "database content should match");
    }

    #[tokio::test]
    async fn restore_checkpoint_to_fresh_workspace_validates_restored_state() {
        // Create a checkpoint
        let checkpoint = TempDir::new().expect("failed to create checkpoint temp dir");
        let checkpoint_path = checkpoint.path();
        let checkpoint_beads = checkpoint_path.join(".beads");
        fs::create_dir_all(&checkpoint_beads).expect("failed to create checkpoint .beads");

        // Create essential bead-forge files
        fs::write(checkpoint_beads.join("beads.db"), b"database")
            .expect("failed to write db");
        fs::write(checkpoint_beads.join("issues.jsonl"), b"issues")
            .expect("failed to write jsonl");

        // Create pointer file
        let pointer_file = checkpoint_path.join("current.json");
        fs::write(&pointer_file, r#"{"type":"test_checkpoint"}"#)
            .expect("failed to write pointer file");

        // Restore to fresh workspace
        let (_temp_workspace, workspace_path) =
            restore_checkpoint_to_fresh_workspace(checkpoint_path)
                .await
                .expect("restore failed");

        // Function validates that .beads directory exists
        assert!(
            workspace_path.join(".beads").exists(),
            "restored .beads directory should exist"
        );

        // Function validates that beads.db exists
        assert!(
            workspace_path.join(".beads/beads.db").exists(),
            "restored database file should exist"
        );
    }

    #[tokio::test]
    async fn flush_and_restore_to_fresh_workspace_roundtrip() {
        // Create a mock workspace with some content
        let workspace = TempDir::new().expect("failed to create workspace temp dir");
        let workspace_path = workspace.path();
        let beads_dir = workspace_path.join(".beads");
        fs::create_dir_all(&beads_dir).expect("failed to create .beads");

        // Create multiple files to test preservation
        let db_file = beads_dir.join("beads.db");
        fs::write(&db_file, b"database content").expect("failed to write db");

        let jsonl_file = beads_dir.join("issues.jsonl");
        fs::write(&jsonl_file, b"line1\nline2\nline3").expect("failed to write jsonl");

        let events_file = beads_dir.join("events.jsonl");
        fs::write(&events_file, b"event1\nevent2").expect("failed to write events");

        // Flush checkpoint
        let (_checkpoint_temp, checkpoint_path) = flush_checkpoint_to_temp(workspace_path)
            .await
            .expect("flush failed");

        // Restore to new fresh workspace
        let (_workspace_temp, restored_path) =
            restore_checkpoint_to_fresh_workspace(&checkpoint_path)
                .await
                .expect("restore failed");

        // Verify all files were preserved in the restored workspace
        assert!(
            restored_path.join(".beads/beads.db").exists(),
            "database file should be restored"
        );
        assert!(
            restored_path.join(".beads/issues.jsonl").exists(),
            "jsonl file should be restored"
        );
        assert!(
            restored_path.join(".beads/events.jsonl").exists(),
            "events file should be restored"
        );

        // Verify content matches
        let original_db = fs::read_to_string(db_file).expect("failed to read original db");
        let restored_db =
            fs::read_to_string(restored_path.join(".beads/beads.db")).expect("failed to read restored db");
        assert_eq!(original_db, restored_db, "database content should match");

        let original_jsonl =
            fs::read_to_string(jsonl_file).expect("failed to read original jsonl");
        let restored_jsonl = fs::read_to_string(restored_path.join(".beads/issues.jsonl"))
            .expect("failed to read restored jsonl");
        assert_eq!(original_jsonl, restored_jsonl, "jsonl content should match");

        let original_events =
            fs::read_to_string(events_file).expect("failed to read original events");
        let restored_events = fs::read_to_string(restored_path.join(".beads/events.jsonl"))
            .expect("failed to read restored events");
        assert_eq!(original_events, restored_events, "events content should match");
    }
}
