use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn main() {
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
    let bead_id = "bf-debug-4390q";
    let outcome = runner.run_with_bead_trace(bead_id).unwrap();

    println!("Test success: {}", outcome.success());
    println!("Exit code: {:?}", outcome.exit_code);
    println!("Stdout length: {}", outcome.stdout.len());
    println!("Stderr length: {}", outcome.stderr.len());

    // Verify trace files exist
    let bead_trace_dir = workspace.join(".beads").join("traces").join(bead_id);
    println!("Trace dir: {}", bead_trace_dir.display());
    println!("Trace dir exists: {}", bead_trace_dir.exists());

    let stdout_path = bead_trace_dir.join("stdout.txt");
    let stderr_path = bead_trace_dir.join("stderr.txt");
    let test_output_path = bead_trace_dir.join("test-output.txt");

    println!("stdout.txt exists: {}", stdout_path.exists());
    println!("stderr.txt exists: {}", stderr_path.exists());
    println!("test-output.txt exists: {}", test_output_path.exists());

    if test_output_path.exists() {
        let test_output_content = fs::read_to_string(&test_output_path).unwrap();
        println!("test-output.txt content length: {}", test_output_content.len());
        println!("test-output.txt first 500 chars:");
        println!("{}", &test_output_content.chars().take(500).collect::<String>());
    }
}
