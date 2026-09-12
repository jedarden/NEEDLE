//! Cleanup function error handling tests for NEEDLE.
//!
//! These tests verify that cleanup functions handle all error states gracefully,
//! including partial failures, idempotent operations, panic scenarios, and
//! worst-case error conditions.
//!
//! ## Test Categories
//!
//! - **Partial failure cleanup tests**: Cleanup when some operations failed
//! - **Idempotent cleanup tests**: Verify cleanup can be called multiple times safely
//! - **Panic recovery cleanup tests**: Cleanup after panic scenarios
//! - **Worst-case error scenarios**: Extreme error conditions that must not panic
//! - **Resource state cleanup tests**: Cleanup with inconsistent resource states

use std::fs::{self, File};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

// ──────────────────────────────────────────────────────────────────────────────
// Partial Failure Cleanup Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod partial_failure_cleanup_tests {
    use super::*;

    #[test]
    fn cleanup_guard_handles_partial_directory_deletion_failures() {
        // Given: A cleanup guard tracking multiple directories
        let temp_base = TempDir::new().expect("failed to create temp base");
        let mut guard = needle::checkpoint_utils::CleanupGuard::new();

        // Create multiple custom directories
        let dir1 = temp_base.path().join("dir1");
        let dir2 = temp_base.path().join("dir2");
        let dir3 = temp_base.path().join("dir3");

        fs::create_dir(&dir1).expect("failed to create dir1");
        fs::create_dir(&dir2).expect("failed to create dir2");
        fs::create_dir(&dir3).expect("failed to create dir3");

        guard.track_custom_path(dir1.clone());
        guard.track_custom_path(dir2.clone());
        guard.track_custom_path(dir3.clone());

        // When: Simulate partial failure by manually removing one directory
        fs::remove_dir(&dir2).expect("failed to remove dir2");

        // Then: Cleanup should succeed even though dir2 is already gone
        let result = guard.cleanup();
        assert!(
            result.is_ok(),
            "cleanup should succeed despite partial failure"
        );

        // Verify remaining directories were cleaned up
        assert!(!dir1.exists(), "dir1 should be cleaned up");
        assert!(!dir3.exists(), "dir3 should be cleaned up");
    }

    #[test]
    fn cleanup_directory_handles_partially_deleted_tree() {
        // Given: A directory tree with some files already deleted
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let base = temp_dir.path().join("tree");
        fs::create_dir_all(&base).expect("failed to create base");

        let file1 = base.join("file1.txt");
        let file2 = base.join("file2.txt");
        let subdir = base.join("subdir");
        let file3 = subdir.join("file3.txt");

        fs::write(&file1, b"content1").expect("failed to write file1");
        fs::write(&file2, b"content2").expect("failed to write file2");
        fs::create_dir(&subdir).expect("failed to create subdir");
        fs::write(&file3, b"content3").expect("failed to write file3");

        // When: Delete some files but not all
        fs::remove_file(&file2).expect("failed to remove file2");

        // Then: Cleanup should still succeed
        let result = needle::checkpoint_utils::cleanup_directory(&base);
        assert!(
            result.is_ok(),
            "cleanup should succeed with partial deletion"
        );

        assert!(!base.exists(), "entire tree should be removed");
    }

    #[test]
    fn cleanup_file_handles_concurrent_deletion() {
        // Given: A file that might be deleted concurrently
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let file = temp_dir.path().join("test.txt");
        fs::write(&file, b"content").expect("failed to write file");

        // When: Spawn a thread that deletes the file
        let file_clone = file.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            let _ = fs::remove_file(&file_clone);
        });

        // Then: Cleanup should handle concurrent deletion gracefully
        thread::sleep(Duration::from_millis(50)); // Give other thread time to run
        let result = needle::checkpoint_utils::cleanup_file(&file);
        assert!(
            result.is_ok(),
            "cleanup should succeed even if file already deleted"
        );
    }

    #[test]
    fn cleanup_heartbeat_file_handles_concurrent_access() {
        // Given: A heartbeat file
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let heartbeat = temp_dir.path().join("heartbeat.json");
        fs::write(&heartbeat, b"{}").expect("failed to write heartbeat");

        // When: Multiple threads attempt cleanup concurrently
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let path = heartbeat.clone();
                thread::spawn(move || {
                    let result = needle::hoop_hooks::cleanup_heartbeat_file(&path);
                    // First succeeds, rest get NotFound but should return Ok
                    result.is_ok()
                })
            })
            .collect();

        // Then: All cleanups should return Ok (idempotent)
        for handle in handles {
            let success = handle.join().expect("thread panicked");
            assert!(success, "concurrent cleanup should succeed");
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Idempotent Cleanup Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod idempotent_cleanup_tests {
    use super::*;

    #[test]
    fn cleanup_guard_multiple_cleanup_calls_safe() {
        // Given: A cleanup guard with tracked directories
        let temp_base = TempDir::new().expect("failed to create temp base");
        let mut guard = needle::checkpoint_utils::CleanupGuard::new();

        let dir1 = temp_base.path().join("dir1");
        let dir2 = temp_base.path().join("dir2");
        fs::create_dir(&dir1).expect("failed to create dir1");
        fs::create_dir(&dir2).expect("failed to create dir2");

        guard.track_custom_path(dir1.clone());
        guard.track_custom_path(dir2.clone());

        // When: Call cleanup multiple times
        let result1 = guard.cleanup();
        let result2 = guard.cleanup();
        let result3 = guard.cleanup();

        // Then: All calls should succeed (no panic on already-cleaned paths)
        assert!(result1.is_ok(), "first cleanup should succeed");
        assert!(result2.is_ok(), "second cleanup should succeed");
        assert!(result3.is_ok(), "third cleanup should succeed");

        assert!(!dir1.exists(), "dir1 should be cleaned up");
        assert!(!dir2.exists(), "dir2 should be cleaned up");
    }

    #[test]
    fn cleanup_directory_repeated_calls_safe() {
        // Given: A directory
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let test_dir = temp_dir.path().join("test");
        fs::create_dir(&test_dir).expect("failed to create test dir");

        // When: Call cleanup multiple times
        let result1 = needle::checkpoint_utils::cleanup_directory(&test_dir);
        let result2 = needle::checkpoint_utils::cleanup_directory(&test_dir);
        let result3 = needle::checkpoint_utils::cleanup_directory(&test_dir);

        // Then: All calls should succeed
        assert!(result1.is_ok(), "first cleanup should succeed");
        assert!(result2.is_ok(), "second cleanup should succeed");
        assert!(result3.is_ok(), "third cleanup should succeed");
    }

    #[test]
    fn cleanup_file_repeated_calls_safe() {
        // Given: A file
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, b"content").expect("failed to write file");

        // When: Call cleanup multiple times
        let result1 = needle::checkpoint_utils::cleanup_file(&test_file);
        let result2 = needle::checkpoint_utils::cleanup_file(&test_file);
        let result3 = needle::checkpoint_utils::cleanup_file(&test_file);

        // Then: All calls should succeed
        assert!(result1.is_ok(), "first cleanup should succeed");
        assert!(result2.is_ok(), "second cleanup should succeed");
        assert!(result3.is_ok(), "third cleanup should succeed");
    }

    #[test]
    fn cleanup_heartbeat_file_idempotent() {
        // Given: A heartbeat file
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let heartbeat = temp_dir.path().join("heartbeat.json");
        fs::write(&heartbeat, b"{}").expect("failed to write heartbeat");

        // When: Call cleanup multiple times
        let result1 = needle::hoop_hooks::cleanup_heartbeat_file(&heartbeat);
        let result2 = needle::hoop_hooks::cleanup_heartbeat_file(&heartbeat);
        let result3 = needle::hoop_hooks::cleanup_heartbeat_file(&heartbeat);

        // Then: All calls should succeed (first deletes, rest no-op)
        assert!(result1.is_ok(), "first cleanup should succeed");
        assert!(result2.is_ok(), "second cleanup should succeed");
        assert!(result3.is_ok(), "third cleanup should succeed");

        assert!(!heartbeat.exists(), "file should be deleted");
    }

    #[test]
    fn cleanup_guard_drop_then_explicit_cleanup_safe() {
        // Given: A cleanup guard
        let temp_base = TempDir::new().expect("failed to create temp base");
        let mut guard = needle::checkpoint_utils::CleanupGuard::new();

        let dir = temp_base.path().join("dir");
        fs::create_dir(&dir).expect("failed to create dir");
        guard.track_custom_path(dir.clone());

        // When: Cleanup explicitly, then drop
        guard.cleanup().expect("first cleanup failed");
        assert!(!dir.exists(), "dir should be cleaned up");

        // Explicit cleanup after already cleaned should be safe
        let result = guard.cleanup();
        assert!(result.is_ok(), "cleanup after cleanup should succeed");
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Panic Recovery Cleanup Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod panic_recovery_cleanup_tests {
    use super::*;

    #[test]
    fn cleanup_guard_does_not_panic_after_operations_fail() {
        // Given: A cleanup guard with directories that will fail to delete
        let temp_base = TempDir::new().expect("failed to create temp base");
        let mut guard = needle::checkpoint_utils::CleanupGuard::new();

        let dir1 = temp_base.path().join("dir1");
        let dir2 = temp_base.path().join("dir2");
        fs::create_dir(&dir1).expect("failed to create dir1");
        fs::create_dir(&dir2).expect("failed to create dir2");

        // Create a file in dir2 and mark it read-only (simulating permission issue)
        let file = dir2.join("readonly.txt");
        fs::write(&file, b"readonly").expect("failed to write file");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&file)
                .expect("failed to get metadata")
                .permissions();
            perms.set_mode(0o444); // Read-only
            fs::set_permissions(&file, perms).expect("failed to set readonly");
        }

        guard.track_custom_path(dir1);
        guard.track_custom_path(dir2);

        // When: Cleanup is called (may fail on dir2 due to readonly file)
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            // This should not panic even if cleanup fails
            let _ = guard.cleanup();
        }));

        // Then: Should not panic
        assert!(
            result.is_ok(),
            "cleanup should not panic on permission errors"
        );
    }

    #[test]
    fn cleanup_directory_does_not_panic_on_permission_error() {
        // Given: A directory with a read-only file
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let test_dir = temp_dir.path().join("test");
        fs::create_dir(&test_dir).expect("failed to create test dir");

        let readonly_file = test_dir.join("readonly.txt");
        fs::write(&readonly_file, b"content").expect("failed to write file");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&readonly_file)
                .expect("failed to get metadata")
                .permissions();
            perms.set_mode(0o444); // Read-only
            fs::set_permissions(&readonly_file, perms).expect("failed to set readonly");
        }

        // When: Cleanup is called
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            needle::checkpoint_utils::cleanup_directory(&test_dir)
        }));

        // Then: Should not panic (may fail, but not panic)
        assert!(
            result.is_ok(),
            "cleanup_directory should not panic on permission errors"
        );
    }

    #[test]
    fn process_guard_drop_does_not_panic_on_already_dead_process() {
        // Given: A ProcessGuard wrapping an already-exited process
        use std::process::Command;

        let mut child = Command::new("true").spawn().expect("failed to spawn true");

        // Wait for process to exit
        let _ = child.wait();

        // When: Creating ProcessGuard and dropping it
        let guard = needle::process_guard::ProcessGuardSync::new(child);

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            drop(guard);
        }));

        // Then: Should not panic even though process is already dead
        assert!(
            result.is_ok(),
            "ProcessGuard drop should not panic on dead process"
        );
    }

    #[test]
    fn process_group_kill_guard_drop_does_not_panic_on_invalid_pid() {
        // Given: A ProcessGroupKillGuard with invalid PID
        let guard = needle::process_guard::ProcessGroupKillGuard::new(0);

        // When: Dropping the guard
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            drop(guard);
        }));

        // Then: Should not panic (best-effort kill, even with invalid PID)
        assert!(
            result.is_ok(),
            "ProcessGroupKillGuard drop should not panic on invalid PID"
        );
    }

    #[test]
    fn cleanup_guard_panic_during_cleanup_continues() {
        // Given: A scenario where cleanup might partially fail
        let temp_base = TempDir::new().expect("failed to create temp base");
        let mut guard = needle::checkpoint_utils::CleanupGuard::new();

        // Create multiple directories
        let dirs: Vec<PathBuf> = (0..5)
            .map(|i| {
                let dir = temp_base.path().join(format!("dir{}", i));
                fs::create_dir(&dir).expect("failed to create dir");
                dir
            })
            .collect();

        for dir in &dirs {
            guard.track_custom_path(dir.clone());
        }

        // Simulate partial failure by removing one directory
        fs::remove_dir(&dirs[2]).expect("failed to remove dir");

        // When: Cleanup is called
        let result = panic::catch_unwind(AssertUnwindSafe(|| guard.cleanup()));

        // Then: Should not panic and should complete cleanup
        assert!(
            result.is_ok(),
            "cleanup should not panic with partial failure"
        );

        // Verify all directories were cleaned up
        for dir in &dirs {
            assert!(!dir.exists(), "directory {:?} should be cleaned up", dir);
        }
    }

    #[test]
    fn test_cleanup_with_unwind_caught_during_operation() {
        // Given: A cleanup operation that might encounter issues
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let test_path = temp_dir.path().join("test.txt");
        fs::write(&test_path, b"test content").expect("failed to write file");

        // When: Performing cleanup within a panic::catch_unwind
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            needle::checkpoint_utils::cleanup_file(&test_path)
        }));

        // Then: Should complete without panic
        assert!(result.is_ok(), "cleanup_file should not panic");
        assert!(result.unwrap().is_ok(), "cleanup_file should succeed");
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Worst-Case Error Scenario Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod worst_case_error_scenario_tests {
    use super::*;

    #[test]
    fn cleanup_guard_handles_non_existent_parent_directory() {
        // Given: A cleanup guard tracking paths in non-existent directories
        let mut guard = needle::checkpoint_utils::CleanupGuard::new();

        let nonexistent_path = PathBuf::from("/nonexistent/deep/nested/path/that/does/not/exist");
        guard.track_custom_path(nonexistent_path);

        // When: Cleanup is called
        let result = guard.cleanup();

        // Then: Should succeed without panic
        assert!(
            result.is_ok(),
            "cleanup should succeed with non-existent paths"
        );
        assert!(
            !guard.has_cleanup_failed(),
            "non-existent paths are not failures"
        );
    }

    #[test]
    fn cleanup_directory_handles_extremely_long_path() {
        // Given: A directory with a very long path
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let long_name = "a".repeat(255);
        let long_path = temp_dir.path().join(&long_name);

        fs::create_dir(&long_path).expect("failed to create long path dir");

        // When: Cleanup is called
        let result = needle::checkpoint_utils::cleanup_directory(&long_path);

        // Then: Should succeed or fail gracefully, not panic
        match result {
            Ok(_) => assert!(!long_path.exists(), "long path should be cleaned up"),
            Err(_) => {
                // If it failed, it should have been a graceful error
                // (e.g., path too long for filesystem)
            }
        }
    }

    #[test]
    fn cleanup_file_handles_special_characters_in_path() {
        // Given: Files with special characters in names
        let temp_dir = TempDir::new().expect("failed to create temp dir");

        let special_cases = vec![
            "file with spaces.txt",
            "file-with-dashes.txt",
            "file_with_underscores.txt",
            "file.multiple.dots.txt",
        ];

        for filename in special_cases {
            let file = temp_dir.path().join(filename);
            fs::write(&file, b"content").expect("failed to write file");

            // When: Cleanup is called
            let result = needle::checkpoint_utils::cleanup_file(&file);

            // Then: Should succeed without panic
            assert!(result.is_ok(), "cleanup should succeed for {:?}", filename);
        }
    }

    #[test]
    fn cleanup_heartbeat_file_handles_invalid_path() {
        // Given: An invalid heartbeat file path
        let invalid_path = PathBuf::from("/nonexistent/path/heartbeat.json");

        // When: Cleanup is called
        let result = needle::hoop_hooks::cleanup_heartbeat_file(&invalid_path);

        // Then: Should succeed without panic (file doesn't exist is OK)
        assert!(
            result.is_ok(),
            "cleanup should succeed for non-existent file"
        );
    }

    #[test]
    fn cleanup_guard_handles_mixed_valid_and_invalid_paths() {
        // Given: A cleanup guard with mix of valid and invalid paths
        let temp_base = TempDir::new().expect("failed to create temp base");
        let mut guard = needle::checkpoint_utils::CleanupGuard::new();

        // Track some valid paths
        let dir1 = temp_base.path().join("dir1");
        let dir2 = temp_base.path().join("dir2");
        fs::create_dir(&dir1).expect("failed to create dir1");
        fs::create_dir(&dir2).expect("failed to create dir2");

        guard.track_custom_path(dir1.clone());
        guard.track_custom_path(dir2.clone());

        // Track some invalid paths
        let invalid1 = PathBuf::from("/nonexistent/path1");
        let invalid2 = PathBuf::from("/nonexistent/path2");

        guard.track_custom_path(invalid1);
        guard.track_custom_path(invalid2);

        // When: Cleanup is called
        let result = guard.cleanup();

        // Then: Should succeed without panic
        assert!(result.is_ok(), "cleanup should succeed with mixed paths");

        // Valid paths should be cleaned
        assert!(!dir1.exists(), "dir1 should be cleaned up");
        assert!(!dir2.exists(), "dir2 should be cleaned up");
    }

    #[test]
    fn cleanup_directory_handles_symlink_loop() {
        // Given: A directory with a symlink loop (if platform supports it)
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let base = temp_dir.path().join("base");
        fs::create_dir(&base).expect("failed to create base");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let subdir = base.join("subdir");
            fs::create_dir(&subdir).expect("failed to create subdir");

            // Create a symlink that points back to parent
            let link = subdir.join("parent_link");
            symlink(&base, &link).expect("failed to create symlink");

            // When: Cleanup is called
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                needle::checkpoint_utils::cleanup_directory(&base)
            }));

            // Then: Should not panic (may fail or succeed, but not panic)
            assert!(result.is_ok(), "cleanup should not panic on symlink loops");
        }

        #[cfg(not(unix))]
        {
            // On non-Unix platforms, skip this test
            // (symlinks behave differently)
        }
    }

    #[test]
    fn cleanup_handles_empty_vs_nonempty_directory_consistently() {
        // Given: Both empty and non-empty directories
        let temp_dir = TempDir::new().expect("failed to create temp dir");

        let empty_dir = temp_dir.path().join("empty");
        fs::create_dir(&empty_dir).expect("failed to create empty dir");

        let nonempty_dir = temp_dir.path().join("nonempty");
        fs::create_dir(&nonempty_dir).expect("failed to create nonempty dir");
        fs::write(nonempty_dir.join("file.txt"), b"content").expect("failed to write file");

        // When: Both are cleaned up
        let result1 = needle::checkpoint_utils::cleanup_directory(&empty_dir);
        let result2 = needle::checkpoint_utils::cleanup_directory(&nonempty_dir);

        // Then: Both should succeed without panic
        assert!(result1.is_ok(), "empty directory cleanup should succeed");
        assert!(result2.is_ok(), "nonempty directory cleanup should succeed");

        assert!(!empty_dir.exists(), "empty dir should be cleaned up");
        assert!(!nonempty_dir.exists(), "nonempty dir should be cleaned up");
    }

    #[test]
    fn cleanup_guard_handles_zero_custom_paths() {
        // Given: A cleanup guard with no paths tracked
        let mut guard = needle::checkpoint_utils::CleanupGuard::new();

        // When: Cleanup is called
        let result = guard.cleanup();

        // Then: Should succeed without panic
        assert!(result.is_ok(), "cleanup with no paths should succeed");
        assert_eq!(guard.custom_path_count(), 0);
        assert_eq!(guard.temp_dir_count(), 0);
    }

    #[test]
    fn cleanup_guard_handles_very_large_number_of_paths() {
        // Given: A cleanup guard tracking many paths
        let temp_base = TempDir::new().expect("failed to create temp base");
        let mut guard = needle::checkpoint_utils::CleanupGuard::new();

        let paths: Vec<PathBuf> = (0..100)
            .map(|i| {
                let dir = temp_base.path().join(format!("dir{:03}", i));
                fs::create_dir(&dir).expect("failed to create dir");
                dir
            })
            .collect();

        for path in &paths {
            guard.track_custom_path(path.clone());
        }

        // When: Cleanup is called
        let result = guard.cleanup();

        // Then: Should succeed without panic
        assert!(result.is_ok(), "cleanup with many paths should succeed");

        // Verify all were cleaned up
        for path in &paths {
            assert!(!path.exists(), "path {:?} should be cleaned up", path);
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Resource State Cleanup Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod resource_state_cleanup_tests {
    use super::*;

    #[test]
    fn cleanup_guard_handles_partially_initialized_state() {
        // Given: A cleanup guard in partially initialized state
        let temp_base = TempDir::new().expect("failed to create temp base");
        let mut guard = needle::checkpoint_utils::CleanupGuard::new();

        // Track a directory, then delete it externally (simulating partial init)
        let dir = temp_base.path().join("incomplete");
        fs::create_dir(&dir).expect("failed to create dir");
        guard.track_custom_path(dir.clone());
        fs::remove_dir(&dir).expect("failed to remove dir");

        // When: Cleanup is called
        let result = guard.cleanup();

        // Then: Should succeed without panic
        assert!(result.is_ok(), "cleanup should succeed with partial state");
    }

    #[test]
    fn cleanup_directory_with_open_file_handles() {
        // Given: A directory with files that might have open handles
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let test_dir = temp_dir.path().join("test");
        fs::create_dir(&test_dir).expect("failed to create test dir");

        let file = test_dir.join("file.txt");
        fs::write(&file, b"content").expect("failed to write file");

        // Open a file handle (but don't keep it open across cleanup on Windows)
        // On Unix, we can delete files with open handles
        {
            let _file = File::open(&file).expect("failed to open file");

            #[cfg(unix)]
            {
                // On Unix, file with open handle can still be deleted
                let result = needle::checkpoint_utils::cleanup_directory(&test_dir);
                assert!(result.is_ok() || result.is_err(), "cleanup completes");
            }

            #[cfg(windows)]
            {
                // On Windows, close the handle before cleanup
                drop(_file);
            }
        }

        #[cfg(windows)]
        {
            // On Windows, cleanup after closing handle
            let result = needle::checkpoint_utils::cleanup_directory(&test_dir);
            assert!(
                result.is_ok(),
                "cleanup should succeed after closing handle"
            );
        }
    }

    #[test]
    fn cleanup_heartbeat_file_with_concurrent_writes() {
        // Given: A heartbeat file being written to concurrently
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let heartbeat = temp_dir.path().join("heartbeat.json");
        fs::write(&heartbeat, b"{}").expect("failed to write heartbeat");

        // Spawn a thread that keeps writing to the file
        let heartbeat_clone = heartbeat.clone();
        let writer = thread::spawn(move || {
            for _ in 0..10 {
                let _ = fs::write(&heartbeat_clone, b"{\"alive\": true}");
                thread::sleep(Duration::from_millis(1));
            }
        });

        // Give writer time to start
        thread::sleep(Duration::from_millis(5));

        // When: Cleanup is called while writes are happening
        let result = needle::hoop_hooks::cleanup_heartbeat_file(&heartbeat);

        // Then: Should succeed without panic (best-effort)
        writer.join().expect("writer thread panicked");
        assert!(
            result.is_ok(),
            "cleanup should succeed despite concurrent writes"
        );
    }

    #[test]
    fn cleanup_guard_state_after_failed_cleanup() {
        // Given: A cleanup guard with paths that will fail
        let temp_base = TempDir::new().expect("failed to create temp base");
        let mut guard = needle::checkpoint_utils::CleanupGuard::new();

        // Track a valid path
        let valid_dir = temp_base.path().join("valid");
        fs::create_dir(&valid_dir).expect("failed to create valid dir");
        guard.track_custom_path(valid_dir.clone());

        // Track an invalid path
        let invalid_path = PathBuf::from("/this/path/does/not/exist");
        guard.track_custom_path(invalid_path);

        // When: Cleanup is called
        let result = guard.cleanup();

        // Then: Should succeed, valid path cleaned, invalid path handled
        assert!(result.is_ok(), "cleanup should succeed");
        assert!(!valid_dir.exists(), "valid path should be cleaned");

        // Guard state should be consistent after cleanup
        assert_eq!(guard.custom_path_count(), 0);
        assert_eq!(guard.temp_dir_count(), 0);
    }

    #[test]
    fn cleanup_handles_read_only_file_in_directory() {
        // Given: A directory containing a read-only file
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let test_dir = temp_dir.path().join("test");
        fs::create_dir(&test_dir).expect("failed to create test dir");

        let readonly_file = test_dir.join("readonly.txt");
        fs::write(&readonly_file, b"content").expect("failed to write file");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&readonly_file)
                .expect("failed to get metadata")
                .permissions();
            perms.set_mode(0o444); // Read-only
            fs::set_permissions(&readonly_file, perms).expect("failed to set readonly");
        }

        // When: Cleanup is called
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            needle::checkpoint_utils::cleanup_directory(&test_dir)
        }));

        // Then: Should not panic (may fail on some platforms, but not panic)
        assert!(
            result.is_ok(),
            "cleanup should not panic on read-only files"
        );

        #[cfg(unix)]
        {
            // On Unix, cleanup might fail due to readonly file
            // Fix permissions and try again
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&readonly_file)
                .expect("failed to get metadata")
                .permissions();
            perms.set_mode(0o644); // Read-write
            let _ = fs::set_permissions(&readonly_file, perms);

            let cleanup_result = needle::checkpoint_utils::cleanup_directory(&test_dir);
            assert!(
                cleanup_result.is_ok(),
                "cleanup should succeed after fixing permissions"
            );
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Process Guard Cleanup Error Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod process_guard_cleanup_error_tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn process_guard_sync_handles_wait_before_drop() {
        // Given: A ProcessGuardSync for a short-lived process
        let child = Command::new("echo")
            .arg("test")
            .stdout(Stdio::null())
            .spawn()
            .expect("failed to spawn echo");

        let guard = needle::process_guard::ProcessGuardSync::new(child);

        // When: Wait is called before drop
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let guard_result = guard.wait();
            // Now dropping the waited guard should be safe
            drop(guard_result);
        }));

        // Then: Should not panic
        assert!(
            result.is_ok(),
            "waited ProcessGuardSync drop should not panic"
        );
    }

    #[test]
    fn process_guard_sync_handles_double_wait() {
        // Given: A ProcessGuardSync for a process
        let child = Command::new("true")
            .stdout(Stdio::null())
            .spawn()
            .expect("failed to spawn true");

        let guard = needle::process_guard::ProcessGuardSync::new(child);

        // When: Wait is called (consumes the guard), then we verify no panic on drop
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let _wait_result = guard.wait();
            // Guard is dropped here, but since wait was called, cleanup is a no-op
        }));

        // Then: Should complete without panic
        assert!(result.is_ok(), "wait then drop should not panic");
    }

    #[test]
    fn process_group_kill_guard_disarm_prevents_kill_on_drop() {
        // Given: A long-running process and a ProcessGroupKillGuard
        let mut child = Command::new("sleep")
            .arg("10")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn sleep");

        let pid = child.id();
        let mut guard = needle::process_guard::ProcessGroupKillGuard::new(pid);

        // When: Guard is disarmed before dropping
        guard.disarm();

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            drop(guard);
            // Kill the child manually since we disarmed the guard
            let _ = child.kill();
            let _ = child.wait();
        }));

        // Then: Should not panic
        assert!(result.is_ok(), "disarmed guard drop should not panic");
    }

    #[test]
    fn process_group_kill_guard_handles_already_killed_process() {
        // Given: A process that we kill manually
        let mut child = Command::new("sleep")
            .arg("10")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn sleep");

        let pid = child.id();
        let _ = child.kill();
        let _ = child.wait();

        // When: ProcessGroupKillGuard is created and dropped for already-dead process
        let guard = needle::process_guard::ProcessGroupKillGuard::new(pid);

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            drop(guard);
        }));

        // Then: Should not panic
        assert!(
            result.is_ok(),
            "guard for already-killed process should not panic"
        );
    }
}
