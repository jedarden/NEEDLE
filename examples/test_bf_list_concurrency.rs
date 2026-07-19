//! Test to reproduce bf list failures under concurrent access.
//!
//! This test attempts to reproduce SQLite lock contention by calling
//! bf list from multiple concurrent tasks, simulating multi-worker
//! scenarios that trigger the recurring "bf list failed" errors.

use anyhow::Context;
use std::path::PathBuf;
use std::process::Command;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Find the NEEDLE workspace
    let workspace = PathBuf::from("/home/coding/NEEDLE");

    // Find the bf binary
    let bf_path = which::which("bf")
        .or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            let candidate = PathBuf::from(format!("{home}/.local/bin/bf"));
            if candidate.exists() {
                Ok(candidate)
            } else {
                Err(anyhow::anyhow!("bf not found"))
            }
        })
        .expect("bf binary not found - install bead-forge");

    println!("Workspace: {:?}", workspace);
    println!("bf binary: {:?}", bf_path);

    // Test 1: Single call to establish baseline
    println!("\n=== Test 1: Single bf list call ===");
    let single_result = run_bf_list(&bf_path, &workspace, 1);
    println!("Single call result: {:?}", single_result);

    // Test 2: Concurrent calls (simulating multi-worker)
    println!("\n=== Test 2: 10 concurrent bf list calls ===");
    let handles: Vec<_> = (0..10)
        .map(|i| {
            let bf_path = bf_path.clone();
            let workspace = workspace.clone();
            tokio::spawn(async move { run_bf_list(&bf_path, &workspace, i) })
        })
        .collect();

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let success_count = results.iter().filter(|r| r.is_ok()).count();
    let failure_count = results.iter().filter(|r| r.is_err()).count();

    println!(
        "Results: {} success, {} failure",
        success_count, failure_count
    );

    // Print details of failures
    for (i, result) in results.iter().enumerate() {
        if let Err(e) = result {
            println!("Task {} failed: {}", i, e);
        }
    }

    // Test 3: More aggressive concurrency
    println!("\n=== Test 3: 50 concurrent bf list calls (aggressive) ===");
    let handles: Vec<_> = (0..50)
        .map(|i| {
            let bf_path = bf_path.clone();
            let workspace = workspace.clone();
            tokio::spawn(async move { run_bf_list(&bf_path, &workspace, i) })
        })
        .collect();

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let success_count = results.iter().filter(|r| r.is_ok()).count();
    let failure_count = results.iter().filter(|r| r.is_err()).count();

    println!(
        "Results: {} success, {} failure",
        success_count, failure_count
    );

    // Print details of failures
    for (i, result) in results.iter().enumerate() {
        if let Err(e) = result {
            println!("Task {} failed: {}", i, e);
            // Check if it's a lock error
            if e.to_string().contains("locked") {
                println!("  -> This is a lock error (SQLite contention)");
            }
        }
    }

    Ok(())
}

fn run_bf_list(
    bf_path: &PathBuf,
    workspace: &PathBuf,
    task_id: usize,
) -> Result<String, anyhow::Error> {
    let output = Command::new(bf_path)
        .args(["list", "--json", "--limit", "999999"])
        .current_dir(workspace)
        .output()
        .with_context(|| format!("bf list spawn failed (task {})", task_id))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        anyhow::bail!(
            "bf list exited with code {} (task {})\nstderr: {}\nstdout: {}",
            code,
            task_id,
            stderr,
            stdout.chars().take(200).collect::<String>()
        );
    }

    Ok(stdout)
}
