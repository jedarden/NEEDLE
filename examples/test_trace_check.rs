use needle::cargo_test::CargoTest;
use std::fs;
use std::path::Path;

fn main() {
    let workspace = Path::new("/tmp/test_trace_workspace");
    let _ = fs::remove_dir_all(workspace);
    fs::create_dir_all(workspace).unwrap();

    // Create a minimal Cargo project
    let cargo_toml = workspace.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-trace-example"
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
        assert!(true);
    }
}
"#,
    )
    .unwrap();

    // Run cargo test with bead trace capture
    let runner = CargoTest::new(workspace);
    let bead_id = "bf-test-trace-check";
    let outcome = runner.run_with_bead_trace(bead_id).unwrap();

    println!("Test success: {}", outcome.success());
    println!("Exit code: {:?}", outcome.exit_code);
    println!("Duration: {:?}", outcome.duration);
    println!("Compilation failed: {}", outcome.compilation_failed);
    println!("Compilation errors: {}", outcome.compilation_errors.len());

    // Check what files were created
    let bead_trace_dir = workspace.join(".beads").join("traces").join(bead_id);
    println!("\nTrace directory: {}", bead_trace_dir.display());
    println!("Trace dir exists: {}", bead_trace_dir.exists());

    if bead_trace_dir.exists() {
        let entries = fs::read_dir(&bead_trace_dir).unwrap();
        println!("\nFiles in trace directory:");
        for entry in entries {
            let entry = entry.unwrap();
            println!("  - {}", entry.file_name().to_string_lossy());
        }

        // Check for test_metrics.json
        let test_metrics_path = bead_trace_dir.join("test_metrics.json");
        println!("\ntest_metrics.json exists: {}", test_metrics_path.exists());
        if test_metrics_path.exists() {
            let content = fs::read_to_string(&test_metrics_path).unwrap();
            println!("test_metrics.json content:\n{}", content);
        }

        // Check for compilation_errors.json
        let errors_path = bead_trace_dir.join("compilation_errors.json");
        println!("compilation_errors.json exists: {}", errors_path.exists());
        if errors_path.exists() {
            let content = fs::read_to_string(&errors_path).unwrap();
            println!("compilation_errors.json content:\n{}", content);
        }
    }
}
