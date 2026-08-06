//! Test to verify bead bf-4390q implementation: test-output.txt creation

use std::fs;
use tempfile::TempDir;

#[test]
fn verify_test_output_txt_created() {
    let temp_dir = TempDir::new().unwrap();
    let workspace = temp_dir.path();

    // Create a minimal Cargo project
    let cargo_toml = workspace.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-4390q"
version = "0.1.0"
edition = "2021"

[lib]
doctest = false
"#,
    )
    .unwrap();

    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).unwrap();

    let lib_rs = src_dir.join("lib.rs");
    fs::write(
        &lib_rs,
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn test_example() {
        println!("Test output message");
        eprintln!("Test error message");
        assert!(true);
    }
}
"#,
    )
    .unwrap();

    // Run cargo test with bead trace capture
    let runner = needle::cargo_test::CargoTest::new(workspace);
    let bead_id = "bf-verify-4390q";
    let outcome = runner.run_with_bead_trace(bead_id).unwrap();

    // Verify test completed
    assert!(outcome.success() || outcome.exit_code.is_some());

    // Verify trace files exist
    let bead_trace_dir = workspace.join(".beads").join("traces").join(bead_id);
    assert!(bead_trace_dir.exists(), "trace directory should exist");

    let stdout_path = bead_trace_dir.join("stdout.txt");
    let stderr_path = bead_trace_dir.join("stderr.txt");
    let test_output_path = bead_trace_dir.join("test-output.txt");

    // All files should exist
    assert!(stdout_path.exists(), "stdout.txt should exist");
    assert!(stderr_path.exists(), "stderr.txt should exist");
    assert!(
        test_output_path.exists(),
        "test-output.txt should exist (bf-4390q deliverable)"
    );

    // Verify test-output.txt has combined content
    let test_output_content =
        fs::read_to_string(&test_output_path).expect("test-output.txt should be readable");

    // Should contain at least one section header (stdout or stderr)
    assert!(
        test_output_content.contains("=== STDOUT ===")
            || test_output_content.contains("=== STDERR ==="),
        "test-output.txt should contain at least one section header (STDOUT or STDERR)"
    );

    // Should contain the STDOUT section header (cargo test always produces stdout)
    assert!(
        test_output_content.contains("=== STDOUT ==="),
        "test-output.txt should contain STDOUT section header"
    );

    // Verify combined output contains test content
    // (cargo test output will include the test result even if custom messages aren't present)
    assert!(
        !test_output_content.is_empty(),
        "test-output.txt should not be empty"
    );
}
