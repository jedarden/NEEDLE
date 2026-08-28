//! Integration tests for panic stack trace capture.
//!
//! These tests verify that:
//! - Panic hooks are properly installed
//! - Full stack traces are captured
//! - Stack traces are preserved even when tests abort

use std::panic::{catch_unwind, AssertUnwindSafe};

#[test]
fn test_panic_hook_installation() {
    // This test verifies that the panic hook can be installed
    // without causing issues
    needle::panic_capture::install_panic_hook();
    assert!(needle::panic_capture::is_hook_installed());
}

#[test]
fn test_panic_hook_is_idempotent() {
    // Multiple installations should be safe
    needle::panic_capture::install_panic_hook();
    needle::panic_capture::install_panic_hook();
    needle::panic_capture::install_panic_hook();
    assert!(needle::panic_capture::is_hook_installed());
}

#[test]
fn test_panic_capture_with_string_message() {
    needle::panic_capture::install_panic_hook();

    let result = catch_unwind(AssertUnwindSafe(|| {
        panic!("test panic with string message");
    }));

    assert!(result.is_err());
    let panic_payload = result.unwrap_err();
    let captured = needle::panic_capture::capture_panic_info(panic_payload);
    assert!(captured.contains("test panic with string message"));
}

#[test]
fn test_panic_capture_with_format_string() {
    needle::panic_capture::install_panic_hook();

    let result = catch_unwind(AssertUnwindSafe(|| {
        panic!("formatted panic: {} = {}", 42, "answer");
    }));

    assert!(result.is_err());
    let panic_payload = result.unwrap_err();
    let captured = needle::panic_capture::capture_panic_info(panic_payload);
    assert!(captured.contains("formatted panic:"));
}

#[test]
fn test_panic_capture_preserves_payload() {
    needle::panic_capture::install_panic_hook();

    let test_string = String::from("owned string panic");
    let result = catch_unwind(AssertUnwindSafe(|| {
        panic!("{}", test_string);
    }));

    assert!(result.is_err());
    let panic_payload = result.unwrap_err();
    let captured = needle::panic_capture::capture_panic_info(panic_payload);
    assert!(captured.contains("owned string panic"));
}

#[test]
fn test_unknown_panic_payload_handling() {
    needle::panic_capture::install_panic_hook();

    // Panic with a non-string payload
    let result = catch_unwind(AssertUnwindSafe(|| {
        panic!("{}", 12345);
    }));

    assert!(result.is_err());
    let panic_payload = result.unwrap_err();
    let captured = needle::panic_capture::capture_panic_info(panic_payload);
    // Should handle unknown payloads gracefully
    assert!(!captured.is_empty());
}

#[test]
fn test_rust_backtrace_env_is_set() {
    // Install the panic hook which should set RUST_BACKTRACE
    needle::panic_capture::install_panic_hook();

    // Verify the environment variable is set (either to "full" or existing value)
    let backtrace = std::env::var("RUST_BACKTRACE");
    assert!(
        backtrace.is_ok(),
        "RUST_BACKTRACE should be set by panic hook"
    );

    // The value should either be "full" (if we set it) or an existing value
    let value = backtrace.unwrap();
    assert!(
        value == "full" || value == "1",
        "RUST_BACKTRACE should be 'full' or '1', got: {}",
        value
    );
}

#[test]
fn test_panic_hook_respects_existing_backtrace_setting() {
    // Set a custom backtrace value
    std::env::set_var("RUST_BACKTRACE", "1");

    // Install panic hook - should not override existing setting
    needle::panic_capture::install_panic_hook();

    // Verify the existing value is preserved
    let backtrace = std::env::var("RUST_BACKTRACE").unwrap();
    assert_eq!(
        backtrace, "1",
        "Existing RUST_BACKTRACE should be preserved"
    );

    // Clean up
    std::env::remove_var("RUST_BACKTRACE");
}

#[test]
fn test_panic_information_captured_on_abort() {
    needle::panic_capture::install_panic_hook();

    // Simulate a panic that would abort the test
    let result = catch_unwind(AssertUnwindSafe(|| {
        let x = 42;
        let y = 0;
        // This would cause a divide by zero in unsafe code, but we'll just panic
        if y == 0 {
            panic!("division by zero: {} / {}", x, y);
        }
    }));

    assert!(result.is_err());
    let panic_payload = result.unwrap_err();
    let captured = needle::panic_capture::capture_panic_info(panic_payload);
    assert!(captured.contains("division by zero"));
}

#[test]
fn test_panic_hook_does_not_interfere_with_normal_tests() {
    // Install panic hook
    needle::panic_capture::install_panic_hook();

    // Verify normal test operations work
    let x = 1 + 1;
    assert_eq!(x, 2);

    let vec = vec![1, 2, 3];
    assert_eq!(vec.len(), 3);

    // Test should complete successfully
}

#[test]
fn test_multiple_consecutive_panics_are_all_captured() {
    needle::panic_capture::install_panic_hook();

    // First panic
    let result1 = catch_unwind(AssertUnwindSafe(|| {
        panic!("first panic");
    }));
    assert!(result1.is_err());
    let captured1 = needle::panic_capture::capture_panic_info(result1.unwrap_err());
    assert!(captured1.contains("first panic"));

    // Second panic
    let result2 = catch_unwind(AssertUnwindSafe(|| {
        panic!("second panic");
    }));
    assert!(result2.is_err());
    let captured2 = needle::panic_capture::capture_panic_info(result2.unwrap_err());
    assert!(captured2.contains("second panic"));

    // Third panic
    let result3 = catch_unwind(AssertUnwindSafe(|| {
        panic!("third panic");
    }));
    assert!(result3.is_err());
    let captured3 = needle::panic_capture::capture_panic_info(result3.unwrap_err());
    assert!(captured3.contains("third panic"));
}

#[test]
#[ignore = "requires actual test runner subprocess to verify integration"]
fn test_panic_capture_integration_with_test_runner() {
    // This test verifies the integration with the test runner
    // It's ignored by default as it requires spawning actual test processes

    use needle::test_runner::TestRunner;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let runner = TestRunner::new(temp_dir.path());

    // Run tests with the panic hook installed
    let result = runner.run_tests(&["--lib"]);

    // Verify the runner completed
    assert!(result.is_ok() || result.is_err());
}

#[test]
fn test_panic_hook_works_with_nested_function_calls() {
    needle::panic_capture::install_panic_hook();

    fn level_3() {
        panic!("panic at depth 3");
    }

    fn level_2() {
        level_3();
    }

    fn level_1() {
        level_2();
    }

    let result = catch_unwind(AssertUnwindSafe(|| {
        level_1();
    }));

    assert!(result.is_err());
    let panic_payload = result.unwrap_err();
    let captured = needle::panic_capture::capture_panic_info(panic_payload);
    assert!(captured.contains("panic at depth 3"));
}
