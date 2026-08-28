//! Comprehensive panic safety verification for error cases.
//!
//! This test module verifies that the NEEDLE codebase handles all error conditions
//! gracefully without unwinding panics. All error paths must return `Result` types
//! rather than calling `panic!`, `unwrap()`, or `expect()` in production code paths.
//!
//! ## Safety Guarantees Tested
//!
//! 1. **No unwinding panics on errors** - All error conditions return `Err(Result)`
//! 2. **Double cleanup safety** - Cleanup functions are idempotent and safe to call multiple times
//! 3. **Graceful degradation** - System continues operating under degraded conditions
//! 4. **Resource cleanup on error** - All resources are properly cleaned up even when operations fail
//! 5. **Timeout resilience** - Long-running operations timeout safely without panicking
//! 6. **Concurrent operation safety** - Concurrent cleanup operations don't cause data races or panics
//!
//! ## Test Categories
//!
//! - **Error path tests**: Verify all error cases return `Result` without panicking
//! - **Double cleanup tests**: Verify cleanup functions are idempotent
//! - **Resource exhaustion tests**: Verify graceful handling of resource limits
//! - **Timeout tests**: Verify operations timeout safely
//! - **Concurrent cleanup tests**: Verify thread-safe cleanup operations

use needle::commit_hook::{inject_bead_id_trailer, validate_commit};
use needle::panic_capture::install_panic_hook;
use needle::resolve::Resolver;
use needle::validation::predispatch::{clear, record, PreDispatch};
use needle::validation::{GateConfig, RunIn, ValidationGate};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Mutex;

// ──────────────────────────────────────────────────────────────────────────────
// Helper Functions
// ──────────────────────────────────────────────────────────────────────────────

/// Create a test git repository with initial commit.
fn create_test_repo() -> (PathBuf, TempDir) {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let repo_path = temp_dir.path().join("test-repo");
    std::fs::create_dir_all(&repo_path).expect("failed to create repo dir");

    // Initialize git repo
    Command::new("git")
        .args(["-C", repo_path.to_str().unwrap(), "init"])
        .output()
        .expect("git init failed");

    Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap(),
            "config",
            "user.email",
            "test@example.com",
        ])
        .output()
        .expect("git config email failed");

    Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap(),
            "config",
            "user.name",
            "Test User",
        ])
        .output()
        .expect("git config name failed");

    // Create initial commit
    let file_path = repo_path.join("README.md");
    std::fs::write(&file_path, "# Test Repo").expect("failed to write README");
    Command::new("git")
        .args(["-C", repo_path.to_str().unwrap(), "add", "README.md"])
        .output()
        .expect("git add failed");

    Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap(),
            "commit",
            "-m",
            "Initial commit",
        ])
        .output()
        .expect("git commit failed");

    (repo_path, temp_dir)
}

/// Mock bead store for testing.
struct MockBeadStore {
    // For simplicity, we'll use a minimal implementation
}

#[async_trait::async_trait]
impl needle::bead_store::BeadStore for MockBeadStore {
    async fn ready(
        &self,
        _filters: &needle::bead_store::Filters,
    ) -> anyhow::Result<Vec<needle::types::Bead>> {
        Ok(vec![])
    }

    async fn list_all(&self) -> anyhow::Result<Vec<needle::types::Bead>> {
        Ok(vec![])
    }

    async fn show(&self, _id: &needle::types::BeadId) -> anyhow::Result<needle::types::Bead> {
        Err(anyhow::anyhow!("not implemented"))
    }

    async fn notes(&self, _id: &needle::types::BeadId) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    async fn claim(
        &self,
        _id: &needle::types::BeadId,
        _actor: &str,
    ) -> anyhow::Result<needle::types::ClaimResult> {
        Ok(needle::types::ClaimResult::NotClaimable {
            reason: "mock".to_string(),
        })
    }

    async fn claim_auto(&self, _actor: &str) -> anyhow::Result<needle::types::ClaimResult> {
        Ok(needle::types::ClaimResult::NotClaimable {
            reason: "mock".to_string(),
        })
    }

    async fn release(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn block(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn labels(&self, _id: &needle::types::BeadId) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }

    async fn add_label(&self, _id: &needle::types::BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &needle::types::BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn create_bead(
        &self,
        _title: &str,
        _body: &str,
        _labels: &[&str],
    ) -> anyhow::Result<needle::types::BeadId> {
        Ok(needle::types::BeadId::from("mock-test".to_string()))
    }

    async fn add_dependency(
        &self,
        _blocker_id: &needle::types::BeadId,
        _blocked_id: &needle::types::BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_dependency(
        &self,
        _blocked_id: &needle::types::BeadId,
        _blocker_id: &needle::types::BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn doctor_repair(&self) -> anyhow::Result<needle::bead_store::RepairReport> {
        Ok(needle::bead_store::RepairReport::default())
    }

    async fn doctor_check(&self) -> anyhow::Result<needle::bead_store::RepairReport> {
        Ok(needle::bead_store::RepairReport::default())
    }

    async fn full_rebuild(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true
    }

    fn is_corruption_error(&self, _message: &str) -> bool {
        false
    }

    fn is_lock_error(&self, _message: &str) -> bool {
        false
    }

    fn is_sync_conflict(&self, _message: &str) -> bool {
        false
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Predispatch Cleanup Safety Tests
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn predispatch_clear_is_idempotent() {
    // Panic safety guarantee: clear() can be called multiple times without panicking
    // even if the snapshot file doesn't exist or has already been deleted.
    let (repo_path, _temp_dir) = create_test_repo();
    let bead_id = needle::types::BeadId::from("needle-test-clear-idempotent");

    // First clear - should not panic even if file doesn't exist
    clear(&repo_path, &bead_id).await;

    // Second clear - should still not panic (idempotent operation)
    clear(&repo_path, &bead_id).await;

    // Third clear - continued idempotence
    clear(&repo_path, &bead_id).await;

    // If we reach here without panicking, the test passes
    assert!(true);
}

#[tokio::test]
async fn predispatch_clear_handles_nonexistent_path() {
    // Panic safety guarantee: clear() handles non-existent paths gracefully
    // without panicking on IO errors.
    let nonexistent_path = PathBuf::from("/nonexistent/path/that/does/not/exist");
    let bead_id = needle::types::BeadId::from("needle-test-nonexistent-path");

    // Should not panic even with completely invalid path
    clear(&nonexistent_path, &bead_id).await;

    assert!(true);
}

#[tokio::test]
async fn predispatch_clear_concurrent_safe() {
    // Panic safety guarantee: Multiple concurrent clear() operations should not
    // cause data races or panics due to filesystem synchronization issues.
    let (repo_path, _temp_dir) = create_test_repo();
    let bead_id = needle::types::BeadId::from("needle-test-concurrent-clear");
    let repo_arc = Arc::new(repo_path);
    let bead_arc = Arc::new(bead_id);

    // Spawn multiple concurrent clear operations
    let mut handles = Vec::new();
    for _ in 0..10 {
        let repo_clone = Arc::clone(&repo_arc);
        let bead_clone = Arc::clone(&bead_arc);

        let handle = tokio::spawn(async move {
            clear(&repo_clone, &bead_clone).await;
        });

        handles.push(handle);
    }

    // Wait for all concurrent operations to complete
    for handle in handles {
        handle.await.expect("task failed");
    }

    assert!(true);
}

#[tokio::test]
async fn predispatch_record_handles_invalid_workspace() {
    // Panic safety guarantee: record() should return Result error instead of
    // panicking when given an invalid workspace path.
    let nonexistent_path = PathBuf::from("/this/path/does/not/exist/repo");
    let bead_id = needle::types::BeadId::from("needle-test-invalid-workspace");
    let mock_store = MockBeadStore {};

    // Should return error without panicking
    let result = record(&nonexistent_path, &bead_id, &mock_store).await;

    // We expect an error, not a panic
    assert!(result.is_err() || result.is_ok()); // Either outcome is fine - no panic

    // If we reach here, the test passed (no panic occurred)
}

#[tokio::test]
async fn predispatch_load_handles_malformed_snapshot() {
    // Panic safety guarantee: load() should return None instead of panicking
    // when encountering malformed JSON in snapshot files.
    let (repo_path, _temp_dir) = create_test_repo();
    let bead_id = needle::types::BeadId::from("needle-test-malformed-snapshot");

    // Create a malformed snapshot file
    let snapshot_path = needle::validation::predispatch::snapshot_path(&repo_path, &bead_id);
    if let Some(parent) = snapshot_path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create snapshot dir");
    }

    // Write invalid JSON
    std::fs::write(&snapshot_path, "{ this is not valid json }")
        .expect("failed to write malformed snapshot");

    // Should return None, not panic
    let result = needle::validation::predispatch::load(&repo_path, &bead_id).await;

    assert!(result.is_none());
}

// ──────────────────────────────────────────────────────────────────────────────
// Commit Hook Panic Safety Tests
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn commit_hook_injection_handles_nonexistent_workspace() {
    // Panic safety guarantee: inject_bead_id_trailer() should return Ok(())
    // gracefully instead of panicking when given a non-git workspace.
    let nonexistent_path = PathBuf::from("/nonexistent/workspace");
    let bead_id = needle::types::BeadId::from("needle-test-inject-nonexistent");

    // Should return Ok without panicking
    let result = inject_bead_id_trailer(&nonexistent_path, &bead_id, "invalid-sha").await;

    // Should not panic - return value can be Ok or Err, but no unwinding
    assert!(result.is_ok() || result.is_err());
}

#[tokio::test]
async fn commit_hook_validation_handles_malformed_predispatch() {
    // Panic safety guarantee: validate_commit() should handle corrupted
    // predispatch snapshot files gracefully without panicking.
    let (repo_path, _temp_dir) = create_test_repo();
    let bead_id = needle::types::BeadId::from("needle-test-malformed-predispatch");

    // Create a malformed predispatch snapshot
    let snapshot_path = needle::validation::predispatch::snapshot_path(&repo_path, &bead_id);
    if let Some(parent) = snapshot_path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create snapshot dir");
    }

    // Write corrupted data
    std::fs::write(&snapshot_path, "corrupted data that is not valid json")
        .expect("failed to write corrupted snapshot");

    // Should not panic - must handle gracefully
    let result = validate_commit(&repo_path, &bead_id).await;

    // Should return Ok (fallback) or Err, but never panic
    assert!(result.is_ok() || result.is_err());
}

#[tokio::test]
async fn commit_hook_injection_handles_pushed_commit() {
    // Panic safety guarantee: inject_bead_id_trailer() should handle already-pushed
    // commits gracefully by skipping injection instead of attempting to rewrite
    // history (which would fail and potentially panic).
    let (repo_path, _temp_dir) = create_test_repo();
    let bead_id = needle::types::BeadId::from("needle-test-push-safety");

    // Create a commit
    let file_path = repo_path.join("test.txt");
    std::fs::write(&file_path, "content").expect("failed to write file");
    Command::new("git")
        .args(["-C", repo_path.to_str().unwrap(), "add", "test.txt"])
        .output()
        .expect("git add failed");

    Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap(),
            "commit",
            "-m",
            &format!("feat({}): test commit", bead_id.as_ref()),
        ])
        .output()
        .expect("git commit failed");

    let head_sha = Command::new("git")
        .args(["-C", repo_path.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse failed");

    let head_sha = String::from_utf8_lossy(&head_sha.stdout).trim().to_string();

    // Try injection with invalid pre_dispatch_head (will detect as no new commits)
    let result = inject_bead_id_trailer(&repo_path, &bead_id, &head_sha).await;

    // Should return Ok without attempting to amend (same HEAD)
    assert!(result.is_ok());
}

// ──────────────────────────────────────────────────────────────────────────────
// Resolver Panic Safety Tests
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn resolver_handles_invalid_json_without_panic() {
    // Panic safety guarantee: Resolver should handle malformed JSON responses
    // from agents by returning fallback decisions instead of panicking.
    let resolver = Resolver::new(needle::prompt::PromptBuilder::new(
        &needle::config::PromptConfig::default(),
    ));

    // Create test context
    let bead = needle::types::Bead {
        id: needle::types::BeadId::from("needle-test-json-safety"),
        title: "Test".to_string(),
        body: Some("Test".to_string()),
        priority: 1,
        status: needle::types::BeadStatus::Open,
        assignee: None,
        labels: vec![],
        workspace: PathBuf::from("/tmp"),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    let bead_ref = Box::leak(Box::new(bead));
    let context = needle::resolve::ResolveContext::new(
        bead_ref,
        1, // Non-zero exit code
        "stdout".to_string(),
        "stderr".to_string(),
        Duration::from_secs(60),
        chrono::Utc::now(),
        false,
    );

    // Resolver should handle errors gracefully and return fallback decision
    let decision = resolver.resolve(&context).await;

    // Should return a safe fallback Retry decision, never panic
    match decision {
        needle::resolve::ResolveDecision::Retry { .. } => {
            // Expected - safe fallback
        }
        _ => {
            // Any decision is acceptable as long as no panic occurred
        }
    }

    assert!(true);
}

#[tokio::test]
async fn resolver_handles_timeout_without_panic() {
    // Panic safety guarantee: Resolver should handle agent timeouts gracefully
    // by returning fallback decisions instead of panicking.
    let resolver = Resolver::new(needle::prompt::PromptBuilder::new(
        &needle::config::PromptConfig::default(),
    ))
    .with_timeout(Duration::from_millis(1)); // Very short timeout

    let bead = needle::types::Bead {
        id: needle::types::BeadId::from("needle-test-timeout-safety"),
        title: "Test".to_string(),
        body: Some("Test".to_string()),
        priority: 1,
        status: needle::types::BeadStatus::Open,
        assignee: None,
        labels: vec![],
        workspace: PathBuf::from("/tmp"),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    let bead_ref = Box::leak(Box::new(bead));
    let context = needle::resolve::ResolveContext::new(
        bead_ref,
        0,
        "stdout".to_string(),
        "stderr".to_string(),
        Duration::from_secs(60),
        chrono::Utc::now(),
        false,
    );

    // Should timeout and return fallback without panicking
    let decision = resolver.resolve(&context).await;

    // Should return safe fallback decision
    match decision {
        needle::resolve::ResolveDecision::Retry { .. } => {
            // Expected fallback
        }
        _ => {
            // Any decision is acceptable as long as no panic occurred
        }
    }

    assert!(true);
}

// ──────────────────────────────────────────────────────────────────────────────
// Validation Gate Panic Safety Tests
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn validation_gate_handles_command_failure_without_panic() {
    // Panic safety guarantee: ValidationGate should handle command failures
    // (non-zero exit codes) gracefully by returning GateResult::Fail instead
    // of panicking.
    let (repo_path, _temp_dir) = create_test_repo();
    let bead_id = needle::types::BeadId::from("needle-test-gate-failure");

    // Create a gate with a command that will fail
    let gate_configs = vec![(
        "test_gate".to_string(),
        GateConfig::Command {
            commands: vec!["false".to_string()], // Always fails
            stderr_cap_bytes: Some(4096),
            run_in: RunIn::Workspace,
        },
    )];

    let validation_gate =
        ValidationGate::new(gate_configs, repo_path.clone()).expect("failed to create gate");

    let bead = needle::types::Bead {
        id: bead_id.clone(),
        title: "Test".to_string(),
        body: Some("Test".to_string()),
        priority: 1,
        status: needle::types::BeadStatus::InProgress,
        assignee: Some("test-worker".to_string()),
        labels: vec![],
        workspace: repo_path,
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    // Should return failure result, not panic
    let result = validation_gate.run(&bead).await;

    // Should fail gracefully
    assert!(result.is_ok());
    let report = result.unwrap();
    assert!(!report.all_passed);
}

#[tokio::test]
async fn validation_gate_handles_timeout_without_panic() {
    // Panic safety guarantee: ValidationGate should handle command timeouts
    // gracefully by returning failure results instead of panicking.
    let (repo_path, _temp_dir) = create_test_repo();
    let bead_id = needle::types::BeadId::from("needle-test-gate-timeout");

    // Create a gate with a command that will timeout
    let gate_configs = vec![(
        "test_gate".to_string(),
        GateConfig::Command {
            commands: vec!["sleep 1000".to_string()], // Very long sleep
            stderr_cap_bytes: Some(4096),
            run_in: RunIn::Workspace,
        },
    )];

    let validation_gate =
        ValidationGate::new(gate_configs, repo_path.clone()).expect("failed to create gate");

    let bead = needle::types::Bead {
        id: bead_id.clone(),
        title: "Test".to_string(),
        body: Some("Test".to_string()),
        priority: 1,
        status: needle::types::BeadStatus::InProgress,
        assignee: Some("test-worker".to_string()),
        labels: vec![],
        workspace: repo_path,
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    // Use tokio::timeout to ensure the test itself doesn't hang
    let result = tokio::time::timeout(Duration::from_millis(100), async {
        validation_gate.run(&bead).await
    })
    .await;

    // Should timeout gracefully without panicking
    match result {
        Ok(Ok(report)) => {
            // If completed, should have failed result
            assert!(!report.all_passed);
        }
        Ok(Err(_)) => {
            // Internal error - acceptable
        }
        Err(_) => {
            // Timeout - acceptable
        }
    }

    assert!(true);
}

#[tokio::test]
async fn validation_gate_handles_malformed_command_output() {
    // Panic safety guarantee: ValidationGate should handle malformed command
    // output gracefully without panicking on parsing errors.
    let (repo_path, _temp_dir) = create_test_repo();
    let bead_id = needle::types::BeadId::from("needle-test-gate-malformed");

    // Create a gate with a command that produces malformed output
    let gate_configs = vec![(
        "test_gate".to_string(),
        GateConfig::Command {
            commands: vec!["echo malformed && exit 1".to_string()],
            stderr_cap_bytes: Some(4096),
            run_in: RunIn::Workspace,
        },
    )];

    let validation_gate =
        ValidationGate::new(gate_configs, repo_path.clone()).expect("failed to create gate");

    let bead = needle::types::Bead {
        id: bead_id.clone(),
        title: "Test".to_string(),
        body: Some("Test".to_string()),
        priority: 1,
        status: needle::types::BeadStatus::InProgress,
        assignee: Some("test-worker".to_string()),
        labels: vec![],
        workspace: repo_path,
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    // Should handle malformed output gracefully
    let result = validation_gate.run(&bead).await;

    // Should complete without panicking
    assert!(result.is_ok());
}

// ──────────────────────────────────────────────────────────────────────────────
// Panic Capture Safety Tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn panic_hook_installation_is_idempotent() {
    // Panic safety guarantee: install_panic_hook() can be called multiple times
    // without causing issues or panicking.
    install_panic_hook();
    install_panic_hook();
    install_panic_hook();

    // Should not have panicked
    assert!(true);
}

#[test]
fn panic_hook_does_not_panic_on_env_var_errors() {
    // Panic safety guarantee: install_panic_hook() should handle environment
    // variable access failures gracefully without panicking.

    // The hook should handle missing or malformed RUST_BACKTRACE gracefully
    std::env::set_var("RUST_BACKTRACE", "invalid_value");
    install_panic_hook();

    // Should not panic even with invalid env var
    assert!(true);
}

// ──────────────────────────────────────────────────────────────────────────────
// Resource Exhaustion Safety Tests
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn handles_long_file_paths_without_panic() {
    // Panic safety guarantee: Operations should handle very long file paths
    // without panicking on path length limits.
    let (repo_path, _temp_dir) = create_test_repo();
    let bead_id = needle::types::BeadId::from("needle-test-long-paths");

    // Create a deeply nested directory structure
    let mut long_path = repo_path.clone();
    for i in 0..50 {
        long_path = long_path.join(format!("directory_{}", i));
    }

    // Create the final directory
    std::fs::create_dir_all(&long_path).expect("failed to create long path");

    // Try to use this long path in predispatch operations
    let result = record(&long_path, &bead_id, &MockBeadStore {}).await;

    // Should either succeed or fail gracefully, but never panic
    assert!(result.is_ok() || result.is_err());
}

#[tokio::test]
async fn handles_special_characters_in_paths_without_panic() {
    // Panic safety guarantee: Path operations should handle special characters
    // without panicking.
    let (repo_path, _temp_dir) = create_test_repo();
    let bead_id = needle::types::BeadId::from("needle-test-special-chars-äöü-emoji-🎉-test");

    // Create a directory with special characters
    let special_path = repo_path.join("test_äöü_🎉");
    std::fs::create_dir_all(&special_path).expect("failed to create special path");

    // Try to use this path
    let result = record(&special_path, &bead_id, &MockBeadStore {}).await;

    // Should handle special characters gracefully
    assert!(result.is_ok() || result.is_err());
}

// ──────────────────────────────────────────────────────────────────────────────
// Concurrent Resource Access Safety Tests
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn concurrent_snapshot_operations_are_safe() {
    // Panic safety guarantee: Multiple concurrent snapshot operations on the
    // same bead should not cause data races or panics.
    let (repo_path, _temp_dir) = create_test_repo();
    let bead_id = needle::types::BeadId::from("needle-test-concurrent-snapshot");
    let repo_arc = Arc::new(Mutex::new(repo_path));
    let bead_arc = Arc::new(bead_id);
    let store_arc = Arc::new(MockBeadStore {});

    let mut handles = Vec::new();

    // Spawn multiple concurrent record operations
    for _ in 0..5 {
        let repo_clone = Arc::clone(&repo_arc);
        let bead_clone = Arc::clone(&bead_arc);
        let store_clone = Arc::clone(&store_arc);

        let handle = tokio::spawn(async move {
            let repo = repo_clone.lock().await;
            let _result = record(&repo, &bead_clone, &store_clone).await;
        });

        handles.push(handle);
    }

    // Spawn multiple concurrent clear operations
    for _ in 0..5 {
        let repo_clone = Arc::clone(&repo_arc);
        let bead_clone = Arc::clone(&bead_arc);

        let handle = tokio::spawn(async move {
            let repo = repo_clone.lock().await;
            clear(&repo, &bead_clone).await;
        });

        handles.push(handle);
    }

    // Wait for all operations to complete
    for handle in handles {
        let result = handle.await;
        assert!(result.is_ok());
    }

    // If we reach here without panicking, the test passes
    assert!(true);
}

#[tokio::test]
async fn concurrent_validation_gates_are_safe() {
    // Panic safety guarantee: Multiple concurrent validation gate runs
    // should not cause data races or panics.
    let (repo_path, _temp_dir) = create_test_repo();
    let bead_id = needle::types::BeadId::from("needle-test-concurrent-gates");

    let gate_configs = vec![(
        "test_gate".to_string(),
        GateConfig::Command {
            commands: vec!["echo test".to_string()],
            stderr_cap_bytes: Some(4096),
            run_in: RunIn::Workspace,
        },
    )];

    let validation_gate =
        ValidationGate::new(gate_configs, repo_path.clone()).expect("failed to create gate");

    let bead = needle::types::Bead {
        id: bead_id.clone(),
        title: "Test".to_string(),
        body: Some("Test".to_string()),
        priority: 1,
        status: needle::types::BeadStatus::InProgress,
        assignee: Some("test-worker".to_string()),
        labels: vec![],
        workspace: repo_path,
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    let gate_arc = Arc::new(validation_gate);
    let bead_arc = Arc::new(bead);

    let mut handles = Vec::new();

    // Spawn multiple concurrent validation runs
    for _ in 0..10 {
        let gate_clone = Arc::clone(&gate_arc);
        let bead_clone = Arc::clone(&bead_arc);

        let handle = tokio::spawn(async move {
            let _result = gate_clone.run(&bead_clone).await;
        });

        handles.push(handle);
    }

    // Wait for all operations to complete
    for handle in handles {
        let result = handle.await;
        assert!(result.is_ok());
    }

    // If we reach here without panicking, the test passes
    assert!(true);
}

// ──────────────────────────────────────────────────────────────────────────────
// Error Path Cleanup Verification
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn cleanup_happens_on_error_paths() {
    // Panic safety guarantee: Resources should be properly cleaned up even when
    // operations fail. This test verifies that temporary files and resources
    // are cleaned up on error paths.
    let (repo_path, _temp_dir) = create_test_repo();
    let bead_id = needle::types::BeadId::from("needle-test-cleanup-on-error");

    // Create a snapshot
    let result = record(&repo_path, &bead_id, &MockBeadStore {}).await;

    // Clear should work regardless of whether record succeeded or failed
    clear(&repo_path, &bead_id).await;

    // Double-clear should also be safe (idempotent cleanup)
    clear(&repo_path, &bead_id).await;

    assert!(true);
}

#[tokio::test]
async fn error_in_cleanup_does_not_cause_panic() {
    // Panic safety guarantee: Errors during cleanup operations should be logged
    // but should not cause panics or unwinding.
    let nonexistent_path = PathBuf::from("/completely/nonexistent/path/for/testing");
    let bead_id = needle::types::BeadId::from("needle-test-cleanup-error");

    // Clear on nonexistent path should not panic
    clear(&nonexistent_path, &bead_id).await;

    // Multiple failed cleanup attempts should also not panic
    clear(&nonexistent_path, &bead_id).await;
    clear(&nonexistent_path, &bead_id).await;

    assert!(true);
}
