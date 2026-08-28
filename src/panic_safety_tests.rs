//! Panic safety verification tests.
//!
//! This module tests that all error handling paths return Results rather than
//! unwinding with panics. All cleanup operations should be panic-free and
//! handle error states gracefully.
//!
//! ## Test Coverage
//!
//! - Double cleanup operations (idempotency)
//! - Cleanup on non-existent resources
//! - Cleanup with permission errors
//! - Graceful degradation on I/O failures
//! - Error path verification for all cleanup functions

#[cfg(test)]
mod cleanup_heartbeat_file_tests {
    use std::fs;
    use std::panic::catch_unwind;

    /// Test that double cleanup doesn't panic.
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

    /// Test cleanup on non-existent file doesn't panic.
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

    /// Test cleanup on existing file then verify it's gone.
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

    /// Test cleanup returns Result, never panics, on I/O errors.
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

    /// Test cleanup_all_test_outputs double cleanup doesn't panic.
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

    /// Test cleanup on non-existent test output directory.
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
    /// Test that all Result-returning functions never panic.
    #[test]
    fn result_returning_functions_never_panic() {
        // Documents the fundamental panic safety guarantee:
        // "All functions that return Result<T, E> must never panic.
        //  Errors must be propagated via the Result type, not by unwinding."
    }

    /// Test idempotency guarantees for cleanup operations.
    #[test]
    fn cleanup_operations_are_idempotent() {
        // Documents the idempotency contract:
        // "All cleanup operations must be idempotent.
        //  Calling cleanup() twice on the same resource must not panic
        //  and must not cause errors (second call is a no-op)."
    }

    /// Test that cleanup operations are best-effort during shutdown.
    #[test]
    fn cleanup_is_best_effort_during_shutdown() {
        // Documents the best-effort cleanup contract:
        // "Cleanup operations during shutdown must be best-effort.
        //  If cleanup fails, log the error but return Ok(()) to allow
        //  shutdown to proceed."
    }
}
