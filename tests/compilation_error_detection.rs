//! Integration tests for compilation error detection.
//!
//! These tests verify that compilation errors are properly detected and parsed
//! from cargo test output in real workspace scenarios.

use std::fs;
use tempfile::TempDir;

use needle::cargo_test::{CargoTest, CompilationErrorVariant};

/// Test that type mismatch errors (E0308) are detected correctly.
#[test]
fn test_type_mismatch_error_detection() {
    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path();

    // Create a Cargo.toml
    let cargo_toml = workspace.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-type-mismatch"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
    )
    .unwrap();

    // Create src directory
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create lib.rs with a type mismatch error
    let lib_rs = src_dir.join("lib.rs");
    fs::write(
        &lib_rs,
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_type_mismatch() {
        let x: i32 = "hello"; // Type mismatch: expected i32, found &str
    }
}
"#,
    )
    .unwrap();

    // Run cargo test
    let runner = CargoTest::new(workspace);
    let outcome = runner.run().unwrap();

    // Verify compilation failure was detected
    assert!(outcome.is_compilation_failure());
    assert!(!outcome.is_test_failure());
    assert!(!outcome.timed_out);

    // Verify compilation errors were parsed
    assert!(!outcome.compilation_errors.is_empty());

    // Verify we found the E0308 type mismatch error
    let type_errors: Vec<_> = outcome
        .compilation_errors
        .iter()
        .filter(|e| e.code.as_deref() == Some("E0308"))
        .collect();
    assert!(!type_errors.is_empty(), "should detect E0308 type mismatch");

    // Verify error classification
    assert_eq!(
        type_errors[0].variant,
        CompilationErrorVariant::TypeMismatch
    );

    // Verify error summary includes the error
    let summary = outcome.compilation_error_summary().unwrap();
    assert!(summary.contains("E0308"));
}

/// Test that multiple compilation errors are all detected.
#[test]
fn test_multiple_compilation_errors_detection() {
    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path();

    // Create a Cargo.toml
    let cargo_toml = workspace.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-multiple-errors"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
    )
    .unwrap();

    // Create src directory
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create lib.rs with multiple compilation errors in different test functions
    let lib_rs = src_dir.join("lib.rs");
    fs::write(
        &lib_rs,
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_error_one() {
        let x: i32 = "hello"; // E0308: type mismatch
    }

    #[test]
    fn test_error_two() {
        let s = String::from("test");
        let _moved = s;
        let _used_again = s; // E0382: use of moved value
    }
}
"#,
    )
    .unwrap();

    // Run cargo test
    let runner = CargoTest::new(workspace);
    let outcome = runner.run().unwrap();

    // Verify compilation failure was detected
    assert!(outcome.is_compilation_failure());
    assert!(!outcome.compilation_errors.is_empty());

    // Verify we found multiple errors (at least 2)
    assert!(
        outcome.compilation_errors.len() >= 2,
        "should detect at least 2 compilation errors, found: {}",
        outcome.compilation_errors.len()
    );

    // Verify we found the expected error types
    let has_type_mismatch = outcome
        .compilation_errors
        .iter()
        .any(|e| e.code.as_deref() == Some("E0308"));
    let has_use_of_moved = outcome
        .compilation_errors
        .iter()
        .any(|e| e.code.as_deref() == Some("E0382"));

    assert!(has_type_mismatch, "should detect E0308 type mismatch error");
    assert!(
        has_use_of_moved,
        "should detect E0382 use of moved value error"
    );
}

/// Test that borrow checker errors (E0382, E0502, etc.) are detected.
#[test]
fn test_borrow_checker_error_detection() {
    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path();

    // Create a Cargo.toml
    let cargo_toml = workspace.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-borrow-checker"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
    )
    .unwrap();

    // Create src directory
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create lib.rs with borrow checker error
    let lib_rs = src_dir.join("lib.rs");
    fs::write(
        &lib_rs,
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_borrow_checker() {
        let mut vec = vec![1, 2, 3];
        let first = &vec[0];
        vec.push(4); // E0502: cannot borrow `vec` as mutable while it is also borrowed as immutable
        println!("{}", first);
    }
}
"#,
    )
    .unwrap();

    // Run cargo test
    let runner = CargoTest::new(workspace);
    let outcome = runner.run().unwrap();

    // Verify compilation failure was detected
    assert!(outcome.is_compilation_failure());
    assert!(!outcome.compilation_errors.is_empty());

    // Verify we found a borrow checker error
    let borrow_errors: Vec<_> = outcome
        .compilation_errors
        .iter()
        .filter(|e| matches!(e.variant, CompilationErrorVariant::BorrowChecker))
        .collect();
    assert!(
        !borrow_errors.is_empty(),
        "should detect borrow checker error"
    );
}

/// Test that import/path errors (E0432, E0433, etc.) are detected.
#[test]
fn test_import_path_error_detection() {
    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path();

    // Create a Cargo.toml
    let cargo_toml = workspace.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-import-error"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
    )
    .unwrap();

    // Create src directory
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create lib.rs with import error
    let lib_rs = src_dir.join("lib.rs");
    fs::write(
        &lib_rs,
        r#"#[cfg(test)]
mod tests {
    use nonexistent_module::NonExistentStruct; // E0432: unresolved import

    #[test]
    fn test_import_error() {
        let _s = NonExistentStruct {};
    }
}
"#,
    )
    .unwrap();

    // Run cargo test
    let runner = CargoTest::new(workspace);
    let outcome = runner.run().unwrap();

    // Verify compilation failure was detected
    assert!(outcome.is_compilation_failure());
    assert!(!outcome.compilation_errors.is_empty());

    // Verify we found an import/path error
    let import_errors: Vec<_> = outcome
        .compilation_errors
        .iter()
        .filter(|e| matches!(e.variant, CompilationErrorVariant::ImportOrPath))
        .collect();
    assert!(!import_errors.is_empty(), "should detect import/path error");
}

/// Test that successful compilation without errors is handled correctly.
#[test]
fn test_successful_compilation_no_errors() {
    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path();

    // Create a Cargo.toml
    let cargo_toml = workspace.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-success-compile"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
    )
    .unwrap();

    // Create src directory
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create lib.rs with valid code
    let lib_rs = src_dir.join("lib.rs");
    fs::write(
        &lib_rs,
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_success() {
        assert_eq!(2 + 2, 4);
    }
}
"#,
    )
    .unwrap();

    // Run cargo test
    let runner = CargoTest::new(workspace);
    let outcome = runner.run().unwrap();

    // Verify no compilation failure
    assert!(!outcome.is_compilation_failure());
    assert!(outcome.compilation_errors.is_empty());
    assert!(outcome.success() || outcome.is_test_failure());
}

/// Test that "could not compile" messages are detected.
#[test]
fn test_could_not_compile_detection() {
    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path();

    // Create a Cargo.toml
    let cargo_toml = workspace.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-compile-fail"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
    )
    .unwrap();

    // Create src directory
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create lib.rs with syntax error
    let lib_rs = src_dir.join("lib.rs");
    fs::write(
        &lib_rs,
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_syntax_error() {
        let x = ; // Syntax error
    }
}
"#,
    )
    .unwrap();

    // Run cargo test
    let runner = CargoTest::new(workspace);
    let outcome = runner.run().unwrap();

    // Verify compilation failure was detected
    assert!(outcome.is_compilation_failure());
    assert!(!outcome.compilation_errors.is_empty());

    // Verify at least one error mentions compilation failure
    let has_compile_failure = outcome
        .compilation_errors
        .iter()
        .any(|e| e.message.contains("could not compile") || e.message.contains("aborting"));
    assert!(
        has_compile_failure,
        "should detect 'could not compile' message"
    );
}

/// Test compilation error summary generation.
#[test]
fn test_compilation_error_summary() {
    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path();

    // Create a Cargo.toml
    let cargo_toml = workspace.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-error-summary"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
    )
    .unwrap();

    // Create src directory
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create lib.rs with multiple errors
    let lib_rs = src_dir.join("lib.rs");
    fs::write(
        &lib_rs,
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_errors() {
        let x: i32 = "hello"; // E0308
        let s = String::from("test");
        let _moved = s;
        let _used = s; // E0382
    }
}
"#,
    )
    .unwrap();

    // Run cargo test
    let runner = CargoTest::new(workspace);
    let outcome = runner.run().unwrap();

    // Verify we can get a summary
    let summary = outcome.compilation_error_summary();
    assert!(summary.is_some(), "should have error summary");

    let summary_text = summary.unwrap();
    // Verify summary contains error count
    assert!(
        summary_text.contains("compilation error"),
        "summary should mention errors"
    );

    // Verify the main outcome summary also includes compilation info
    let main_summary = outcome.summary();
    assert!(main_summary.contains("Compilation failed"));
}

/// Test that test failures (not compilation errors) are distinguished.
#[test]
fn test_test_failure_vs_compilation_error() {
    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path();

    // Create a Cargo.toml
    let cargo_toml = workspace.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-test-failure"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
    )
    .unwrap();

    // Create src directory
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create lib.rs with test that fails (but compiles successfully)
    let lib_rs = src_dir.join("lib.rs");
    fs::write(
        &lib_rs,
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_fails() {
        assert!(false, "intentional test failure");
    }
}
"#,
    )
    .unwrap();

    // Run cargo test
    let runner = CargoTest::new(workspace);
    let outcome = runner.run().unwrap();

    // Verify this is a test failure, NOT a compilation error
    assert!(!outcome.is_compilation_failure());
    assert!(outcome.is_test_failure());
    assert!(outcome.compilation_errors.is_empty());
}

/// End-to-end test: Full workflow with compilation errors detected and written to trace.
///
/// This test verifies the complete workflow:
/// 1. Run cargo test with compilation errors
/// 2. Capture output to bead trace directory
/// 3. Verify compilation errors are written to compilation_errors.json
/// 4. Verify errors can be read back and match what was detected
#[test]
fn test_end_to_end_compilation_error_workflow() {
    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path();
    let bead_id = "bf-e2e-test";

    // Create a Cargo.toml
    let cargo_toml = workspace.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-e2e-compilation"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
    )
    .unwrap();

    // Create src directory
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create lib.rs with multiple compilation errors (type mismatch and borrow checker)
    let lib_rs = src_dir.join("lib.rs");
    fs::write(
        &lib_rs,
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_type_error() {
        let x: i32 = "hello"; // E0308: type mismatch
    }

    #[test]
    fn test_borrow_error() {
        let s = String::from("test");
        let _moved = s;
        let _used = s; // E0382: use of moved value
    }
}
"#,
    )
    .unwrap();

    // Step 1: Run cargo test with bead trace capture
    let runner = CargoTest::new(workspace);
    let outcome = runner.run_with_bead_trace(bead_id).unwrap();

    // Step 2: Verify compilation errors were detected in the outcome
    assert!(outcome.is_compilation_failure(), "should detect compilation failure");
    assert!(!outcome.compilation_errors.is_empty(), "should have detected errors");
    assert!(
        outcome.compilation_errors.len() >= 2,
        "should detect at least 2 errors"
    );

    // Verify specific error codes were detected
    let has_type_mismatch = outcome
        .compilation_errors
        .iter()
        .any(|e| e.code.as_deref() == Some("E0308"));
    let has_use_of_moved = outcome
        .compilation_errors
        .iter()
        .any(|e| e.code.as_deref() == Some("E0382"));
    assert!(has_type_mismatch, "should detect E0308 type mismatch");
    assert!(has_use_of_moved, "should detect E0382 use of moved value");

    // Step 3: Verify trace directory was created
    let bead_trace_dir = workspace.join(".beads").join("traces").join(bead_id);
    assert!(bead_trace_dir.exists(), "trace directory should exist");

    // Step 4: Verify all expected files exist
    let stdout_path = bead_trace_dir.join("stdout.txt");
    let stderr_path = bead_trace_dir.join("stderr.txt");
    let metrics_path = bead_trace_dir.join("test_metrics.json");
    let errors_path = bead_trace_dir.join("compilation_errors.json");

    assert!(stdout_path.exists(), "stdout.txt should exist");
    assert!(stderr_path.exists(), "stderr.txt should exist");
    assert!(metrics_path.exists(), "test_metrics.json should exist");
    assert!(errors_path.exists(), "compilation_errors.json should exist");

    // Step 5: Verify stderr contains compilation error messages
    let stderr_content = fs::read_to_string(&stderr_path)
        .expect("stderr should be readable");
    assert!(
        stderr_content.contains("error[E") || stderr_content.contains("could not compile"),
        "stderr should contain compilation error indicators"
    );

    // Step 6: Read and verify test_metrics.json
    let metrics_content = fs::read_to_string(&metrics_path)
        .expect("test_metrics.json should be readable");
    let metrics: serde_json::Value = serde_json::from_str(&metrics_content)
        .expect("test_metrics.json should be valid JSON");

    assert_eq!(
        metrics["exit_code"].as_i64(),
        outcome.exit_code.map(i64::from),
        "metrics should record exit code"
    );
    assert!(
        metrics["duration_ms"].as_u64().is_some(),
        "metrics should have duration"
    );

    // Step 7: Read and verify compilation_errors.json
    let errors_content = fs::read_to_string(&errors_path)
        .expect("compilation_errors.json should be readable");
    let errors_json: serde_json::Value = serde_json::from_str(&errors_content)
        .expect("compilation_errors.json should be valid JSON");

    // Verify it's an array
    assert!(errors_json.is_array(), "compilation errors should be an array");

    let errors_array = errors_json.as_array().unwrap();
    assert!(
        !errors_array.is_empty(),
        "compilation errors array should not be empty"
    );

    // Step 8: Verify the errors match what was detected in the outcome
    let json_has_type_mismatch = errors_array.iter().any(|e| {
        e.get("code").and_then(|c| c.as_str()) == Some("E0308")
    });
    let json_has_use_of_moved = errors_array.iter().any(|e| {
        e.get("code").and_then(|c| c.as_str()) == Some("E0382")
    });

    assert!(
        json_has_type_mismatch,
        "compilation_errors.json should contain E0308 error"
    );
    assert!(
        json_has_use_of_moved,
        "compilation_errors.json should contain E0382 error"
    );

    // Step 9: Verify error structure has expected fields
    for error in errors_array {
        assert!(
            error.get("variant").is_some(),
            "each error should have a variant field"
        );
        assert!(
            error.get("message").is_some(),
            "each error should have a message field"
        );
    }

    // Step 10: Verify summary methods work correctly
    let summary = outcome.compilation_error_summary();
    assert!(summary.is_some(), "should have error summary");
    let summary_text = summary.unwrap();
    assert!(
        summary_text.contains("E0308") || summary_text.contains("E0382"),
        "summary should mention detected error codes"
    );

    let main_summary = outcome.summary();
    assert!(
        main_summary.contains("Compilation failed"),
        "main summary should indicate compilation failure"
    );
}

/// End-to-end test: Full workflow with successful compilation (no errors).
///
/// This test verifies the complete workflow when compilation succeeds.
#[test]
fn test_end_to_end_successful_compilation_workflow() {
    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path();
    let bead_id = "bf-e2e-success";

    // Create a Cargo.toml
    let cargo_toml = workspace.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-e2e-success"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
    )
    .unwrap();

    // Create src directory
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create lib.rs with valid code
    let lib_rs = src_dir.join("lib.rs");
    fs::write(
        &lib_rs,
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_success() {
        assert_eq!(2 + 2, 4);
    }
}
"#,
    )
    .unwrap();

    // Run cargo test with bead trace capture
    let runner = CargoTest::new(workspace);
    let outcome = runner.run_with_bead_trace(bead_id).unwrap();

    // Verify no compilation errors
    assert!(!outcome.is_compilation_failure(), "should not indicate compilation failure");
    assert!(outcome.compilation_errors.is_empty(), "should have no compilation errors");

    // Verify trace directory exists
    let bead_trace_dir = workspace.join(".beads").join("traces").join(bead_id);
    assert!(bead_trace_dir.exists(), "trace directory should exist");

    // Verify all expected files exist
    let stdout_path = bead_trace_dir.join("stdout.txt");
    let stderr_path = bead_trace_dir.join("stderr.txt");
    let metrics_path = bead_trace_dir.join("test_metrics.json");
    let errors_path = bead_trace_dir.join("compilation_errors.json");

    assert!(stdout_path.exists(), "stdout.txt should exist");
    assert!(stderr_path.exists(), "stderr.txt should exist");
    assert!(metrics_path.exists(), "test_metrics.json should exist");

    // compilation_errors.json should NOT exist when there are no errors
    // (or it should be empty based on the implementation)
    if errors_path.exists() {
        let errors_content = fs::read_to_string(&errors_path)
            .expect("compilation_errors.json should be readable");
        let errors_json: serde_json::Value = serde_json::from_str(&errors_content)
            .expect("compilation_errors.json should be valid JSON");
        assert!(
            errors_json.as_array().map(|a| a.is_empty()).unwrap_or(false),
            "compilation_errors.json should be empty array when no errors"
        );
    }

    // Verify no compilation error summary
    assert!(
        outcome.compilation_error_summary().is_none(),
        "should have no compilation error summary when compilation succeeds"
    );
}

/// End-to-end test: Full workflow with test failure (not compilation error).
///
/// This test verifies that test failures are distinguished from compilation errors.
#[test]
fn test_end_to_end_test_failure_workflow() {
    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path();
    let bead_id = "bf-e2e-test-fail";

    // Create a Cargo.toml
    let cargo_toml = workspace.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-e2e-test-failure"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
    )
    .unwrap();

    // Create src directory
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create lib.rs with test that fails (compiles successfully)
    let lib_rs = src_dir.join("lib.rs");
    fs::write(
        &lib_rs,
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_fails() {
        assert!(false, "intentional test failure");
    }
}
"#,
    )
    .unwrap();

    // Run cargo test with bead trace capture
    let runner = CargoTest::new(workspace);
    let outcome = runner.run_with_bead_trace(bead_id).unwrap();

    // Verify this is a test failure, NOT compilation error
    assert!(!outcome.is_compilation_failure(), "should not indicate compilation failure");
    assert!(outcome.is_test_failure(), "should indicate test failure");
    assert!(outcome.compilation_errors.is_empty(), "should have no compilation errors");

    // Verify trace directory exists
    let bead_trace_dir = workspace.join(".beads").join("traces").join(bead_id);
    assert!(bead_trace_dir.exists(), "trace directory should exist");

    // compilation_errors.json should NOT exist for test failures
    let errors_path = bead_trace_dir.join("compilation_errors.json");
    if errors_path.exists() {
        let errors_content = fs::read_to_string(&errors_path)
            .expect("compilation_errors.json should be readable");
        let errors_json: serde_json::Value = serde_json::from_str(&errors_content)
            .expect("compilation_errors.json should be valid JSON");
        assert!(
            errors_json.as_array().map(|a| a.is_empty()).unwrap_or(false),
            "compilation_errors.json should be empty for test failures"
        );
    }
}
