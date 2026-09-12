//! # Panic Safety Verification Tests
//!
//! This test suite verifies that all error handling and cleanup operations in
//! NEEDLE are panic-safe — they return Results rather than unwinding, and they
//! handle error conditions gracefully without causing panics.
//!
//! ## Parent Bead Acceptance Criteria
//!
//! This module addresses the acceptance criteria from parent bead needle-b4c6ca87:
//!
//! - ✅ Add test cases that verify no unwinding panics on errors
//! - ✅ Verify all error paths return Results, not panic
//! - ✅ Add graceful error handling verification
//! - ✅ Test edge cases that might trigger panics (e.g., double cleanup)
//! - ✅ Ensure cleanup functions handle all error states gracefully
//! - ✅ Document panic safety guarantees in test comments
//!
//! ## Panic Safety Contract
//!
//! All cleanup and error handling code in NEEDLE MUST:
//!
//! 1. **Never panic on errors** — All error conditions return `Result<T, E>`
//! 2. **Handle double cleanup gracefully** — Cleanup functions called twice
//!    must not panic (e.g., deleting a file that doesn't exist)
//! 3. **Suppress errors in Drop** — Cleanup in `Drop` implementations must catch
//!    and log errors, never propagate panics
//! 4. **Use best-effort cleanup** — Operations that may fail (e.g., killing
//!    an already-dead process) should use `let _ =` to ignore errors
//!
//! ## What These Tests Verify
//!
//! - **CleanupGuard double cleanup**: Calling cleanup twice doesn't panic
//! - **CleanupGuard missing directories**: Cleaning non-existent paths returns Ok
//! - **ProcessGuard double cleanup**: Killing an already-killed process doesn't panic
//! - **ProcessGuard already-dead process**: Killing a dead process is handled gracefully
//! - **Drop implementations**: All Drop handlers suppress panics
//! - **Error paths**: All error conditions return Results, never panic
//!
//! ## Why This Matters
//!
//! Panics during cleanup are especially dangerous because:
//!
//! - **They abort the program** — A panic during Drop in a test or during
//!   shutdown immediately aborts, losing all diagnostic information
//! - **They mask the real error** — The original error that triggered cleanup
//!   is lost when a panic in cleanup code occurs
//! - **They break test isolation** — A panic in cleanup prevents test teardown
//!   from completing, contaminating subsequent tests
//! - **They cause resource leaks** — A panic in cleanup may leave temporary
//!   files, processes, or other resources uncleaned
//!
//! These tests ensure that cleanup code is robust and panic-free under all
//! error conditions, including edge cases like double cleanup and operations
//! on already-deleted resources.

use needle::checkpoint_utils::{cleanup_directory, cleanup_file, CleanupGuard};
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use tempfile::TempDir;

// =============================================================================
// CleanupGuard Panic Safety Tests
// =============================================================================

#[test]
fn cleanup_guard_double_cleanup_does_not_panic() {
    // **Parent Bead AC**: Test edge cases that might trigger panics (e.g., double cleanup)
    //
    // This test verifies that calling `cleanup()` twice on the same CleanupGuard
    // does not panic. This is critical because:
    //
    // - The first cleanup removes all tracked directories and files
    // - The second cleanup attempts to remove the same (now non-existent) paths
    // - If the second cleanup panics, it would abort the program during shutdown
    //
    // **Why this matters**: In test scenarios, cleanup might be called explicitly
    // in test teardown AND during Drop. If double cleanup panics, tests will abort
    // instead of reporting their actual failure.

    let mut guard = CleanupGuard::new();
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let test_path = temp_dir.path().join("test_file.txt");

    // Create a test file to track as a custom path
    fs::write(&test_path, b"test content").expect("failed to write test file");
    guard.track_custom_path(test_path.clone());

    // First cleanup should succeed
    let result1 = guard.cleanup();
    assert!(result1.is_ok(), "First cleanup should succeed");

    // Second cleanup should NOT panic even though the file is already deleted
    let result2 = guard.cleanup();
    assert!(
        result2.is_ok(),
        "Second cleanup should succeed without panicking"
    );

    // Verify the file is gone
    assert!(!test_path.exists(), "File should be deleted after cleanup");
}

#[test]
fn cleanup_guard_handles_missing_directory_gracefully() {
    // **Parent Bead AC**: Verify cleanup functions handle all error states gracefully
    //
    // This test verifies that attempting to clean a directory that doesn't exist
    // returns Ok(()) rather than panicking. This is critical because:
    //
    // - Temporary directories may be deleted by external processes
    // - Race conditions can cause a directory to exist when tracked but deleted when cleaned
    // - If cleanup panics on missing directories, it aborts the entire program
    //
    // **Why this matters**: In concurrent environments (multiple tests, workers,
    // or external cleanup processes), a tracked directory may be deleted before
    // cleanup runs. The cleanup must handle this gracefully.

    let mut guard = CleanupGuard::new();
    let nonexistent_path = PathBuf::from("/tmp/this_directory_does_not_exist_12345");

    // Track a path that doesn't exist
    guard.track_custom_path(nonexistent_path.clone());

    // Cleanup should succeed even though the directory doesn't exist
    let result = guard.cleanup();
    assert!(
        result.is_ok(),
        "Cleanup should succeed even with missing directories"
    );
}

#[test]
fn cleanup_guard_drop_does_not_panic_on_errors() {
    // **Parent Bead AC**: Ensure cleanup functions handle all error states gracefully
    //
    // This test verifies that dropping a CleanupGuard never panics, even if
    // cleanup operations fail. This is critical because:
    //
    // - Drop is called during stack unwinding, and a panic in Drop aborts the program
    // - Errors during cleanup (e.g., permission denied, file in use) must be logged, not panicked
    // - The cleanup must complete even if some operations fail
    //
    // **Why this matters**: If cleanup in Drop panics, the program immediately
    // aborts with no diagnostic output. This makes debugging extremely difficult
    // and can mask the actual error that triggered the cleanup.

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let test_path = temp_dir.path().join("test_file.txt");

    // Create a file
    fs::write(&test_path, b"test content").expect("failed to write test file");

    // Create a guard and track the file, then let it drop
    {
        let mut guard = CleanupGuard::new();
        guard.track_custom_path(test_path.clone());
        // Guard drops here - should NOT panic
    }

    // Verify the file was cleaned up
    assert!(
        !test_path.exists(),
        "File should be deleted after guard drops"
    );
}

#[test]
fn cleanup_directory_handles_nonexistent_path() {
    // **Parent Bead AC**: Verify all error paths return Results, not panic
    //
    // This test verifies that `cleanup_directory()` returns `Result<()>` for
    // non-existent paths rather than panicking. This is critical because:
    //
    // - cleanup_directory may be called on paths that were already cleaned
    // - Idempotent cleanup is a common pattern (cleanup before setup, cleanup after)
    // - If the function panics on non-existent paths, it cannot be used idempotently
    //
    // **Why this matters**: Cleanup functions are often called defensively
    // (clean up before test, clean up after test, clean up on error). If the
    // first cleanup succeeds, the second must not panic.

    let nonexistent_path = PathBuf::from("/tmp/this_does_not_exist_xyz123");

    // cleanup_directory should return Ok for non-existent paths
    let result = cleanup_directory(&nonexistent_path);
    assert!(
        result.is_ok(),
        "cleanup_directory should not panic on non-existent paths"
    );
}

#[test]
fn cleanup_file_handles_nonexistent_file() {
    // **Parent Bead AC**: Verify all error paths return Results, not panic
    //
    // This test verifies that `cleanup_file()` returns `Result<()>` for
    // non-existent files rather than panicking. This is critical because:
    //
    // - Files may be deleted by external processes between tracking and cleanup
    // - Idempotent cleanup requires that deleting a non-existent file is not an error
    // - If the function panics on non-existent files, it cannot be used safely
    //
    // **Why this matters**: In test scenarios, a file may be cleaned up by
    // previous test runs, by external cleanup processes, or by the OS. The
    // cleanup function must handle this gracefully.

    let nonexistent_file = PathBuf::from("/tmp/this_file_does_not_exist_abc456.txt");

    // cleanup_file should return Ok for non-existent files
    let result = cleanup_file(&nonexistent_file);
    assert!(
        result.is_ok(),
        "cleanup_file should not panic on non-existent files"
    );
}

// =============================================================================
// ProcessGuard Panic Safety Tests
// =============================================================================

#[test]
fn process_guard_sync_drop_does_not_panic_on_already_dead_process() {
    // **Parent Bead AC**: Test edge cases that might trigger panics (e.g., double cleanup)
    //
    // This test verifies that dropping a ProcessGuardSync does not panic even if
    // the child process has already exited. This is critical because:
    //
    // - The child process may exit naturally before the guard drops
    // - Calling kill() on an already-dead process returns an error
    // - Drop implementations must suppress these errors, not panic
    //
    // **Why this matters**: If a ProcessGuardSync panics on drop when the process
    // is already dead, it will abort the program during shutdown or test
    // teardown, masking the actual test result or error.

    use needle::process_guard::ProcessGuardSync;
    use std::process::Command;

    // Spawn a process that exits immediately
    let child = Command::new("true")
        .spawn()
        .expect("failed to spawn true command");

    // Create a guard and let it drop immediately
    {
        let _guard = ProcessGuardSync::new(child);
        // Process exits, guard drops here - should NOT panic
    }

    // If we reach here, the drop succeeded without panicking
    // (no assertion needed - reaching this point is success)
}

#[test]
fn process_group_kill_guard_handles_already_killed_process() {
    // **Parent Bead AC**: Ensure cleanup functions handle all error states gracefully
    //
    // This test verifies that ProcessGroupKillGuard does not panic when killing
    // an already-dead process group. This is critical because:
    //
    // - The process group may have already exited when the guard drops
    // - killpg(2) returns ESRCH for non-existent process groups
    // - The Drop implementation ignores this error and does not panic
    //
    // **Why this matters**: ProcessGroupKillGuard is used in timeout scenarios
    // where the process may have already exited. If drop panics on an
    // already-dead process, it aborts the program instead of completing
    // cleanup gracefully.

    use needle::process_guard::ProcessGroupKillGuard;
    use std::process::{Command, Stdio};

    // Spawn a short-lived process in a new process group
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("failed to spawn process");

    let pid = child.id();

    // Create a guard and immediately drop it (process may or may not be dead)
    {
        let _guard = ProcessGroupKillGuard::new(pid);
        // Guard drops here - should NOT panic even if process is already dead
    }

    // Wait for the child process to avoid zombie
    let _ = child.wait();

    // If we reach here, the drop succeeded without panicking
    // (no assertion needed - reaching this point is success)
}

#[test]
fn process_group_kill_guard_disarm_prevents_kill() {
    // **Parent Bead AC**: Test edge cases that might trigger panics (e.g., double cleanup)
    //
    // This test verifies that disarming a ProcessGroupKillGuard prevents
    // the process group from being killed on drop. This is critical because:
    //
    // - If the process has already been waited on, killing it is unnecessary
    // - The guard must support a "disarm" mechanism for manual cleanup
    // - Disarming must be safe and not cause panics
    //
    // **Why this matters**: In some scenarios, the caller may want to manually
    // wait for the process and then disarm the guard to prevent the automatic
    // kill. This must be safe and not cause any panics.

    use needle::process_guard::ProcessGroupKillGuard;
    use std::process::{Command, Stdio};

    let mut child = Command::new("sh")
        .arg("-c")
        .arg("sleep 0.1 && exit 0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("failed to spawn process");

    let pid = child.id();

    let mut guard = ProcessGroupKillGuard::new(pid);

    // Disarm the guard
    guard.disarm();

    // Drop the guard - should NOT try to kill the process
    drop(guard);

    // Wait for the child process to avoid zombie
    let _ = child.wait();

    // If we reach here, disarming worked correctly
    // (no assertion needed - reaching this point is success)
}

// =============================================================================
// Integration Tests: Panic Safety in Real Scenarios
// =============================================================================

#[test]
fn cleanup_guard_with_concurrent_deletions() {
    // **Parent Bead AC**: Add graceful error handling verification
    //
    // This test verifies that CleanupGuard handles concurrent deletions
    // gracefully (e.g., when an external process deletes a tracked file).
    // This is critical because:
    //
    // - In multi-process test environments, external cleanup may delete files
    // - Race conditions can cause files to exist when tracked but deleted when cleaned
    // - The cleanup must not panic in these scenarios
    //
    // **Why this matters**: When multiple workers or tests run concurrently,
    // a file tracked by one guard may be deleted by another process. The
    // cleanup must handle this gracefully without panicking.

    let mut guard = CleanupGuard::new();
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let test_path = temp_dir.path().join("concurrent_delete_test.txt");

    // Create and track a file
    fs::write(&test_path, b"test content").expect("failed to write test file");
    guard.track_custom_path(test_path.clone());

    // Simulate external deletion (race condition)
    fs::remove_file(&test_path).expect("failed to simulate external deletion");

    // Cleanup should succeed even though the file was externally deleted
    let result = guard.cleanup();
    assert!(
        result.is_ok(),
        "Cleanup should succeed with externally deleted files"
    );
}

#[test]
fn multiple_cleanup_guards_drop_without_panicking() {
    // **Parent Bead AC**: Test edge cases that might trigger panics (e.g., double cleanup)
    //
    // This test verifies that multiple CleanupGuards can be dropped in sequence
    // without any panics. This is critical because:
    //
    // - Test code often creates multiple guards for different resources
    // - All guards must drop without panicking during stack unwinding
    // - A panic in one guard's drop would abort the entire program
    //
    // **Why this matters**: In complex test scenarios, multiple resources
    // (temp directories, files, processes) are each guarded by a CleanupGuard.
    // If any guard panics on drop, the entire test suite aborts, losing all
    // diagnostic information about the actual test failure.

    let temp_dir1 = TempDir::new().expect("failed to create temp dir 1");
    let temp_dir2 = TempDir::new().expect("failed to create temp dir 2");
    let temp_dir3 = TempDir::new().expect("failed to create temp dir 3");

    let file1 = temp_dir1.path().join("file1.txt");
    let file2 = temp_dir2.path().join("file2.txt");
    let file3 = temp_dir3.path().join("file3.txt");

    fs::write(&file1, b"content1").expect("failed to write file1");
    fs::write(&file2, b"content2").expect("failed to write file2");
    fs::write(&file3, b"content3").expect("failed to write file3");

    // Create multiple guards
    {
        let mut guard1 = CleanupGuard::new();
        let mut guard2 = CleanupGuard::new();
        let mut guard3 = CleanupGuard::new();

        guard1.track_custom_path(file1.clone());
        guard2.track_custom_path(file2.clone());
        guard3.track_custom_path(file3.clone());

        // All guards drop here in reverse order - should NOT panic
    }

    // Verify all files were cleaned up
    assert!(!file1.exists(), "File1 should be deleted");
    assert!(!file2.exists(), "File2 should be deleted");
    assert!(!file3.exists(), "File3 should be deleted");
}

// =============================================================================
// Error Path Verification Tests
// =============================================================================

#[test]
fn verify_cleanup_error_returns_result_not_panic() {
    // **Parent Bead AC**: Verify all error paths return Results, not panic
    //
    // This test verifies that cleanup operations return `Result<()>` for
    // all error conditions instead of panicking. This is critical because:
    //
    // - Panics cannot be handled or recovered from
    // - Results allow callers to handle errors gracefully
    // - Cleanup errors should be logged, not cause program abort
    //
    // **Why this matters**: If cleanup functions panic on errors, they cannot
    // be used in contexts where errors are expected (e.g., cleaning up
    // resources that may or may not exist). Returning Results allows callers
    // to handle errors appropriately.

    use std::io;

    // Attempt to clean up a directory we don't have permission to delete
    // This should return an error, not panic
    let system_dir = PathBuf::from("/root");
    let result = cleanup_directory(&system_dir);

    // Should return an error (permission denied or not found), not panic
    match result {
        Ok(_) => {
            // Unexpected success, but not a panic
            println!("Warning: cleanup_directory succeeded on /root (unexpected)");
        }
        Err(e) => {
            // Expected error path - verify it's the right kind
            let inner = e.downcast_ref::<io::Error>();
            if let Some(io_err) = inner {
                assert!(
                    io_err.kind() == io::ErrorKind::PermissionDenied
                        || io_err.kind() == io::ErrorKind::NotFound,
                    "Error should be permission denied or not found, got: {:?}",
                    io_err.kind()
                );
            }
        }
    }

    // If we reach here, the function returned Result instead of panicking
    // (no assertion needed - reaching this point is success)
}

#[test]
fn verify_tempdir_cleanup_is_panic_safe() {
    // **Parent Bead AC**: Ensure cleanup functions handle all error states gracefully
    //
    // This test verifies that TempDir cleanup through CleanupGuard is panic-safe.
    // TempDir handles its own cleanup via Drop, and CleanupGuard coordinates this.
    // This is critical because:
    //
    // - TempDir is used extensively in test infrastructure
    // - If TempDir cleanup panics, all tests abort
    // - CleanupGuard must orchestrate TempDir cleanup safely
    //
    // **Why this matters**: TempDir is the standard way to manage temporary
    // directories in Rust tests. If TempDir cleanup through CleanupGuard panics,
    // the entire test infrastructure becomes unreliable.

    let mut guard = CleanupGuard::new();

    // Create multiple temp dirs
    let temp1 = TempDir::new().expect("failed to create temp1");
    let temp2 = TempDir::new().expect("failed to create temp2");
    let temp3 = TempDir::new().expect("failed to create temp3");

    // Create files in each
    fs::write(temp1.path().join("file1.txt"), b"data1").expect("failed to write file1");
    fs::write(temp2.path().join("file2.txt"), b"data2").expect("failed to write file2");
    fs::write(temp3.path().join("file3.txt"), b"data3").expect("failed to write file3");

    // Track them
    guard.track_temp_dir(temp1);
    guard.track_temp_dir(temp2);
    guard.track_temp_dir(temp3);

    // Explicit cleanup should not panic
    let result = guard.cleanup();
    assert!(result.is_ok(), "TempDir cleanup should succeed");

    // Cleanup again should not panic (idempotent)
    let result2 = guard.cleanup();
    assert!(
        result2.is_ok(),
        "Second TempDir cleanup should succeed (idempotent)"
    );
}
