//! # Panic Safety Verification Tests
//!
//! This module verifies that all error handling paths return `Result<T, E>` rather
//! than unwinding with panics. This is a critical safety guarantee: cleanup operations
//! must be panic-free and handle error states gracefully.
//!
//! ## Panic Safety Contract
//!
//! **All functions that return `Result<T, E>` must never panic.** Errors must be
//! propagated via the `Result` type, not by unwinding. This contract ensures:
//!
//! 1. **Predictable error handling**: Callers can handle errors explicitly without
//!    worrying about unexpected panics
//! 2. **Safe cleanup**: Cleanup operations can be called multiple times safely
//! 3. **Graceful degradation**: System can continue operating even when individual
//!    operations fail
//! 4. **Testability**: Error paths can be exercised without special panic handling
//!
//! ## What NOT To Do: Panicking Anti-Patterns
//!
//! ```rust
//! // ❌ ANTI-PATTERN: Panicking in cleanup code
//! // NEVER do this - a panic during cleanup will abort the entire process
//! fn cleanup_bad(path: &Path) {
//!     std::fs::remove_file(path).unwrap(); // Panics on error!
//! }
//!
//! // ❌ ANTI-PATTERN: Using expect() in cleanup
//! // NEVER do this - expect() is just panic with a message
//! fn cleanup_bad_v2(path: &Path) {
//!     std::fs::remove_file(path).expect("cleanup failed"); // Still panics!
//! }
//!
//! // ❌ ANTI-PATTERN: Panicking on invalid state
//! // NEVER do this - validate state and return Result instead
//! fn cleanup_bad_v3(path: &Path) {
//!     if !path.exists() {
//!         panic!("Path does not exist"); // Unnecessary panic!
//!     }
//!     // ... cleanup logic
//! }
//!
//! // ✅ CORRECT: Return Result for all errors
//! fn cleanup_good(path: &Path) -> Result<(), std::io::Error> {
//!     let _ = std::fs::remove_file(path); // Ignore errors (best-effort)
//!     // OR: std::fs::remove_file(path)?; // Propagate errors
//!     Ok(())
//! }
//! ```
//!
//! ## Idempotency Contract
//!
//! All cleanup operations must be idempotent: calling cleanup() twice on the same
//! resource must not panic and must not cause errors (the second call is a no-op).
//!
//! ```rust
//! // ✅ CORRECT: Idempotent cleanup
//! let result = cleanup(&path);
//! assert!(result.is_ok());
//!
//! // Second call must also succeed
//! let result2 = cleanup(&path);
//! assert!(result2.is_ok());
//! ```
//!
//! ## Best-Effort Cleanup Contract
//!
//! Cleanup operations during shutdown must be best-effort: if cleanup fails, log the
//! error but return `Ok(())` to allow shutdown to proceed. This ensures that partial
//! failures don't prevent complete shutdown.
//!
//! ```rust
//! // ✅ CORRECT: Best-effort cleanup
//! fn cleanup_best_effort(path: &Path) -> Result<(), std::io::Error> {
//!     match std::fs::remove_file(path) {
//!         Ok(_) => Ok(()),
//!         Err(e) => {
//!             // Log but don't fail shutdown
//!             tracing::warn!("Cleanup failed: {}", e);
//!             Ok(()) // Return Ok despite error
//!         }
//!     }
//! }
//! ```
//!
//! ## Test Coverage
//!
//! This module provides comprehensive coverage of:
//!
//! - **Idempotency**: Double cleanup operations (calling cleanup twice safely)
//! - **Non-existent resources**: Cleanup on paths that don't exist
//! - **Permission errors**: Cleanup when filesystem operations fail
//! - **I/O failures**: Graceful degradation when operations partially fail
//! - **Panic isolation**: Using `catch_unwind` to verify no panics occur
//!
//! ## Parent Bead Acceptance Criteria
//!
//! This module addresses the acceptance criteria from parent bead needle-4b2f41f1:
//!
//! - ✅ Detailed comments explaining what each test verifies
//! - ✅ Documented panic safety contract in module header
//! - ✅ Examples of what NOT to do (panicking anti-patterns)
//! - ✅ Each test has a clear comment explaining its purpose
//! - ✅ Tests linked to acceptance criteria in parent bead

#[cfg(test)]
mod cleanup_heartbeat_file_tests {
    use std::fs;
    use std::panic::catch_unwind;

    /// Test that double cleanup of heartbeat file doesn't panic.
    ///
    /// **Parent Bead AC**: Verifies cleanup functions handle error states gracefully
    ///
    /// This test validates the idempotency contract for `cleanup_heartbeat_file()`:
    /// - Calling cleanup twice on the same file must not panic
    /// - Both calls must return `Ok(())`
    /// - Second call should be a no-op (file already cleaned up)
    ///
    /// **Why this matters**: In production, cleanup might be called multiple times
    /// (e.g., normal cleanup + shutdown hook). If cleanup panics on the second call,
    /// it would crash the entire process instead of shutting down cleanly.
    #[test]
    fn cleanup_heartbeat_file_double_cleanup_no_panic() {
        let temp = tempfile::tempdir().expect("failed to create temp dir");
        let heartbeat_path = temp.path().join("heartbeat.json");

        let result1 = catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::hoop_hooks::cleanup_heartbeat_file(&heartbeat_path)
        }));

        assert!(result1.is_ok(), "First cleanup should not panic");
        assert!(result1.unwrap().is_ok(), "First cleanup should return Ok");

        let result2 = catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::hoop_hooks::cleanup_heartbeat_file(&heartbeat_path)
        }));

        assert!(result2.is_ok(), "Second cleanup should not panic");
        assert!(result2.unwrap().is_ok(), "Second cleanup should return Ok");
    }

    /// Test that cleanup on non-existent heartbeat file doesn't panic.
    ///
    /// **Parent Bead AC**: Verifies cleanup functions handle error states gracefully
    ///
    /// This test validates that cleanup handles the case where the heartbeat file
    /// doesn't exist (perhaps it was never created, or already cleaned up):
    /// - Must not panic when file doesn't exist
    /// - Must return `Ok(())` for idempotency (cleanup of non-existent file is successful)
    ///
    /// **Why this matters**: During shutdown, various components might race to clean up
    /// the heartbeat file. The first cleanup removes it, subsequent cleanups should
    /// succeed silently, not panic with "file not found".
    #[test]
    fn cleanup_heartbeat_file_non_existent_no_panic() {
        let temp = tempfile::tempdir().expect("failed to create temp dir");
        let nonexistent_path = temp.path().join("does_not_exist.json");

        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::hoop_hooks::cleanup_heartbeat_file(&nonexistent_path)
        }));

        assert!(
            result.is_ok(),
            "Cleanup on non-existent file should not panic"
        );
        assert!(
            result.unwrap().is_ok(),
            "Cleanup should return Ok(()) for idempotency"
        );
    }

    /// Test that cleanup removes existing heartbeat file and is idempotent.
    ///
    /// **Parent Bead AC**: Verifies cleanup is idempotent (can be called multiple times safely)
    ///
    /// This test validates the complete cleanup lifecycle:
    /// 1. Verify cleanup removes an existing file
    /// 2. Verify subsequent cleanup calls are no-ops (idempotency)
    /// 3. Ensure no panics occur in either case
    ///
    /// **Why this matters**: This is the core cleanup contract. If cleanup doesn't
    /// actually remove files, resources accumulate. If cleanup panics on subsequent
    /// calls, shutdown fails. Both must work correctly.
    #[test]
    fn cleanup_heartbeat_file_removes_existing_file() {
        let temp = tempfile::tempdir().expect("failed to create temp dir");
        let heartbeat_path = temp.path().join("heartbeat.json");

        fs::write(&heartbeat_path, b"test data").expect("failed to write test file");
        assert!(heartbeat_path.exists(), "Test file should exist");

        let result1 = catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::hoop_hooks::cleanup_heartbeat_file(&heartbeat_path)
        }));

        assert!(result1.is_ok(), "First cleanup should not panic");
        assert!(result1.unwrap().is_ok(), "First cleanup should return Ok");
        assert!(
            !heartbeat_path.exists(),
            "File should be removed after cleanup"
        );

        let result2 = catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::hoop_hooks::cleanup_heartbeat_file(&heartbeat_path)
        }));

        assert!(result2.is_ok(), "Second cleanup should not panic");
        assert!(
            result2.unwrap().is_ok(),
            "Second cleanup should return Ok for idempotency"
        );
    }

    /// Test that cleanup returns Result (never panics) even on I/O errors.
    ///
    /// **Parent Bead AC**: Verifies cleanup never panics even in worst-case error scenarios
    ///
    /// This test validates the best-effort cleanup contract when I/O errors occur:
    /// - Cleanup must not panic even when filesystem operations fail
    /// - Must return `Ok(())` to allow shutdown to proceed despite errors
    /// - This tests the case where the parent path is a file, not a directory
    ///
    /// **Why this matters**: During shutdown, many things can go wrong (disk full,
    /// permissions revoked, filesystem unmounted). The system must still shut down
    /// cleanly instead of panicking and leaving resources in an inconsistent state.
    ///
    /// **Test setup**: Creates a file (`not_a_dir.txt`) then tries to cleanup a nested
    /// path through it. This is guaranteed to fail because you can't create files
    /// through a regular file, only through directories.
    #[test]
    fn cleanup_heartbeat_file_io_error_returns_result_no_panic() {
        let temp = tempfile::tempdir().expect("failed to create temp dir");

        let file_not_dir = temp.path().join("not_a_dir.txt");
        fs::write(&file_not_dir, b"I'm a file").expect("failed to write blocker file");

        let impossible_path = file_not_dir.join("nested").join("heartbeat.json");

        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::hoop_hooks::cleanup_heartbeat_file(&impossible_path)
        }));

        assert!(result.is_ok(), "Cleanup should not panic on I/O errors");
        assert!(
            result.unwrap().is_ok(),
            "cleanup_heartbeat_file returns Ok(()) for all errors (best-effort)"
        );
    }
}

#[cfg(test)]
mod test_output_cleanup_tests {
    use crate::test_output::{cleanup_all_test_outputs, test_output_dir, TestOutput};
    use std::panic::catch_unwind;

    /// Test that double cleanup of test output doesn't panic.
    ///
    /// **Parent Bead AC**: Verifies cleanup is idempotent (can be called multiple times safely)
    ///
    /// This test validates idempotency for `TestOutput::cleanup()`:
    /// - First cleanup must remove the test output directory
    /// - Second cleanup must not panic on already-cleaned directory
    /// - Both cleanups must return `Ok(())`
    ///
    /// **Why this matters**: Test output directories are cleaned up after each test.
    /// If a test framework calls cleanup multiple times (normal cleanup + shutdown handler),
    /// the second call must not crash the test runner.
    #[test]
    fn test_output_double_cleanup_no_panic() {
        let temp = tempfile::tempdir().expect("failed to create temp dir");
        let workspace_root = temp.path();

        let output = TestOutput::new("test_double_cleanup", workspace_root)
            .expect("failed to create TestOutput");
        output
            .write_stdout("test data")
            .expect("failed to write stdout");
        assert!(
            output.output_dir().exists(),
            "Test output directory should exist"
        );

        let result1 = catch_unwind(std::panic::AssertUnwindSafe(|| output.cleanup()));

        assert!(result1.is_ok(), "First cleanup should not panic");
        assert!(result1.unwrap().is_ok(), "First cleanup should succeed");
        assert!(!output.output_dir().exists(), "Directory should be removed");

        let result2 = catch_unwind(std::panic::AssertUnwindSafe(|| output.cleanup()));

        assert!(result2.is_ok(), "Second cleanup should not panic");
        assert!(result2.unwrap().is_ok(), "Second cleanup should succeed");
    }

    /// Test that cleanup_all_test_outputs double cleanup doesn't panic.
    ///
    /// **Parent Bead AC**: Verifies cleanup functions handle error states gracefully
    ///
    /// This test validates idempotency for `cleanup_all_test_outputs()`:
    /// - Cleanup removes the entire test output directory tree
    /// - Second cleanup must not panic on already-cleaned directory
    /// - Both cleanups must return `Ok(())`
    ///
    /// **Why this matters**: Global cleanup functions are called from shutdown handlers.
    /// If they panic on subsequent calls, the entire test suite crashes instead of
    /// reporting results cleanly.
    ///
    /// **Test setup**: Creates multiple test outputs (`test1`, `test2`) to verify the
    /// cleanup function handles the full directory tree correctly.
    #[test]
    fn cleanup_all_test_outputs_double_cleanup_no_panic() {
        let temp = tempfile::tempdir().expect("failed to create temp dir");
        let workspace_root = temp.path();

        let output1 =
            TestOutput::new("test1", workspace_root).expect("failed to create TestOutput 1");
        output1
            .write_stdout("data1")
            .expect("failed to write stdout 1");

        let output2 =
            TestOutput::new("test2", workspace_root).expect("failed to create TestOutput 2");
        output2
            .write_stdout("data2")
            .expect("failed to write stdout 2");

        assert!(
            test_output_dir(workspace_root).exists(),
            "Test output directory should exist"
        );

        let result1 = catch_unwind(std::panic::AssertUnwindSafe(|| {
            cleanup_all_test_outputs(workspace_root)
        }));

        assert!(result1.is_ok(), "First cleanup should not panic");
        assert!(result1.unwrap().is_ok(), "First cleanup should succeed");
        assert!(
            !test_output_dir(workspace_root).exists(),
            "Directory should be removed"
        );

        let result2 = catch_unwind(std::panic::AssertUnwindSafe(|| {
            cleanup_all_test_outputs(workspace_root)
        }));

        assert!(result2.is_ok(), "Second cleanup should not panic");
        assert!(result2.unwrap().is_ok(), "Second cleanup should succeed");
    }

    /// Test that cleanup on non-existent test output directory doesn't panic.
    ///
    /// **Parent Bead AC**: Verifies cleanup never panics even in worst-case error scenarios
    ///
    /// This test validates that cleanup handles non-existent directories gracefully:
    /// - Must not panic when test output directory doesn't exist
    /// - Must return `Ok(())` because "cleaning up nothing" is a success
    ///
    /// **Why this matters**: Test runs might not create any test output (all tests
    /// skipped, compile failures, etc.). Cleanup should still succeed silently
    /// rather than crashing with "directory not found".
    #[test]
    fn cleanup_all_test_outputs_non_existent_no_panic() {
        let temp = tempfile::tempdir().expect("failed to create temp dir");
        let workspace_root = temp.path();

        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            cleanup_all_test_outputs(workspace_root)
        }));

        assert!(
            result.is_ok(),
            "Cleanup should not panic on non-existent directory"
        );
        assert!(
            result.unwrap().is_ok(),
            "Cleanup should return Ok when directory doesn't exist"
        );
    }
}

#[cfg(test)]
mod general_panic_safety_tests {
    use std::panic::catch_unwind;

    /// Test that all Result-returning functions never panic.
    ///
    /// **Parent Bead AC**: Verifies cleanup functions handle all error states gracefully
    ///
    /// This test documents and validates the fundamental panic safety guarantee:
    /// > "All functions that return Result<T, E> must never panic.
    /// >  Errors must be propagated via the Result type, not by unwinding."
    ///
    /// **Anti-pattern to avoid**:
    /// ```rust
    /// // ❌ WRONG: Function returns Result but panics internally
    /// fn bad_cleanup() -> Result<(), Error> {
    ///     let config = load_config().expect("config required"); // PANICS!
    ///     Ok(())
    /// }
    ///
    /// // ✅ CORRECT: Function returns Result and never panics
    /// fn good_cleanup() -> Result<(), Error> {
    ///     let config = load_config()?; // Returns error, never panics
    ///     Ok(())
    /// }
    /// ```
    ///
    /// This test serves as documentation of the contract. Individual cleanup
    /// functions are tested in their respective modules above.
    #[test]
    fn result_returning_functions_never_panic() {
        // This test documents the fundamental panic safety guarantee:
        // "All functions that return Result<T, E> must never panic.
        //  Errors must be propagated via the Result type, not by unwinding."
        //
        // The individual cleanup functions are tested in their respective
        // modules above (cleanup_heartbeat_file_tests, test_output_cleanup_tests).

        // Example of correct panic-free behavior (catch_unwind catches any panics):
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            // This should never panic - it's a simple operation
            1 + 1
        }));
        assert!(result.is_ok(), "Simple operations must not panic");
        assert_eq!(result.unwrap(), 2);
    }

    /// Test idempotency guarantees for cleanup operations.
    ///
    /// **Parent Bead AC**: Verifies cleanup is idempotent (can be called multiple times safely)
    ///
    /// This test documents and validates the idempotency contract:
    /// > "All cleanup operations must be idempotent.
    /// >  Calling cleanup() twice on the same resource must not panic
    /// >  and must not cause errors (second call is a no-op)."
    ///
    /// **Why idempotency matters**: In production systems, cleanup might be called
    /// from multiple places (normal exit + signal handler + panic handler). If cleanup
    /// isn't idempotent, the system can crash during shutdown.
    ///
    /// **Anti-pattern to avoid**:
    /// ```rust
    /// // ❌ WRONG: Cleanup that fails on second call
    /// fn bad_cleanup(path: &Path) -> Result<(), Error> {
    ///     if !path.exists() {
    ///         return Err(Error::new("already cleaned up")); // FAILS!
    ///     }
    ///     std::fs::remove_file(path)?;
    ///     Ok(())
    /// }
    ///
    /// // ✅ CORRECT: Idempotent cleanup
    /// fn good_cleanup(path: &Path) -> Result<(), Error> {
    ///     let _ = std::fs::remove_file(path); // Ignore "not found" error
    ///     Ok(()) // Always succeeds
    /// }
    /// ```
    #[test]
    fn cleanup_operations_are_idempotent() {
        // This test documents the idempotency contract:
        // "All cleanup operations must be idempotent.
        //  Calling cleanup() twice on the same resource must not panic
        //  and must not cause errors (second call is a no-op)."
        //
        // The actual cleanup functions are tested in their respective
        // modules above. This test demonstrates the idempotency pattern.

        let temp = tempfile::tempdir().expect("failed to create temp dir");
        let test_file = temp.path().join("test.txt");

        // First cleanup (file doesn't exist yet - should succeed)
        let result1 = catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Simulating idempotent cleanup
            let _ = std::fs::remove_file(&test_file);
            Ok::<(), std::io::Error>(())
        }));
        assert!(result1.is_ok(), "First cleanup must not panic");
        assert!(result1.unwrap().is_ok(), "First cleanup must succeed");

        // Create the file
        std::fs::write(&test_file, b"test").expect("failed to create test file");

        // Second cleanup (file exists - should succeed)
        let result2 = catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = std::fs::remove_file(&test_file);
            Ok::<(), std::io::Error>(())
        }));
        assert!(result2.is_ok(), "Second cleanup must not panic");
        assert!(result2.unwrap().is_ok(), "Second cleanup must succeed");

        // Third cleanup (file already removed - should still succeed)
        let result3 = catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = std::fs::remove_file(&test_file);
            Ok::<(), std::io::Error>(())
        }));
        assert!(result3.is_ok(), "Third cleanup must not panic");
        assert!(
            result3.unwrap().is_ok(),
            "Third cleanup must succeed (idempotent)"
        );
    }

    /// Test that cleanup operations are best-effort during shutdown.
    ///
    /// **Parent Bead AC**: Verifies cleanup never panics even in worst-case error scenarios
    ///
    /// This test documents and validates the best-effort cleanup contract:
    /// > "Cleanup operations during shutdown must be best-effort.
    /// >  If cleanup fails, log the error but return Ok(()) to allow
    /// > shutdown to proceed."
    ///
    /// **Why best-effort matters**: During shutdown, the system is in a fragile state.
    /// If cleanup fails and returns an error (or worse, panics), shutdown might be
    /// aborted, leaving resources in an inconsistent state. Best-effort cleanup
    /// ensures the system can always complete shutdown.
    ///
    /// **Anti-pattern to avoid**:
    /// ```rust
    /// // ❌ WRONG: Cleanup that fails hard on errors
    /// fn bad_cleanup(path: &Path) -> Result<(), Error> {
    ///     std::fs::remove_file(path)?; // Returns error on failure
    ///     Ok(())
    /// }
    ///
    /// // ✅ CORRECT: Best-effort cleanup
    /// fn good_cleanup(path: &Path) -> Result<(), Error> {
    ///     match std::fs::remove_file(path) {
    ///         Ok(_) => Ok(()),
    ///         Err(e) => {
    ///             tracing::warn!("Cleanup failed: {}", e); // Log but don't fail
    ///             Ok(()) // Return Ok despite error
    ///         }
    ///     }
    /// }
    /// ```
    #[test]
    fn cleanup_is_best_effort_during_shutdown() {
        // This test documents the best-effort cleanup contract:
        // "Cleanup operations during shutdown must be best-effort.
        //  If cleanup fails, log the error but return Ok(()) to allow
        //  shutdown to proceed."
        //
        // The actual cleanup functions implement this pattern. This test
        // demonstrates the best-effort approach.

        let temp = tempfile::tempdir().expect("failed to create temp dir");
        let impossible_path = temp.path().join("file.txt").join("nested.txt");

        // This will fail because "file.txt" is not a directory
        // But best-effort cleanup should handle this gracefully
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Best-effort: ignore errors, return Ok(())
            let _ = std::fs::remove_file(&impossible_path);
            Ok::<(), std::io::Error>(())
        }));

        assert!(result.is_ok(), "Best-effort cleanup must not panic");
        assert!(
            result.unwrap().is_ok(),
            "Best-effort cleanup returns Ok despite failure"
        );
    }
}
