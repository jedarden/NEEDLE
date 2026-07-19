//! Test to reproduce bf write operation failures under concurrent access.
//!
//! This test attempts to reproduce SQLite lock contention by calling
//! bf claim (a write operation) from multiple concurrent tasks.

use anyhow::Context;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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

    // First, create a test bead that we can try to claim concurrently
    let test_bead_id = create_test_bead(&bf_path, &workspace)?;
    println!("Created test bead: {}", test_bead_id);

    // Test 1: 10 concurrent claim attempts on the same bead
    println!("\n=== Test 1: 10 concurrent claim attempts on same bead ===");
    let handles: Vec<_> = (0..10)
        .map(|i| {
            let bf_path = bf_path.clone();
            let workspace = workspace.clone();
            let bead_id = test_bead_id.clone();
            let assignee = format!("worker-{}", i);
            tokio::spawn(async move { run_bf_claim(&bf_path, &workspace, &bead_id, &assignee, i) })
        })
        .collect();

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let success_count = results.iter().filter(|r| r.is_ok()).count();
    let failure_count = results.iter().filter(|r| r.is_err()).count();
    let lock_error_count = results
        .iter()
        .filter(|r| {
            if let Err(e) = r {
                e.to_string().contains("locked") || e.to_string().contains("database")
            } else {
                false
            }
        })
        .count();

    println!(
        "Results: {} success, {} failure, {} lock/database errors",
        success_count, failure_count, lock_error_count
    );

    // Print details of failures
    for (i, result) in results.iter().enumerate() {
        if let Err(e) = result {
            println!("Task {} failed: {}", i, e);
            if e.to_string().contains("locked") {
                println!("  -> This is a lock error (SQLite contention)");
            }
        }
    }

    // Test 2: 50 concurrent claim attempts
    println!("\n=== Test 2: 50 concurrent claim attempts ===");
    let handles: Vec<_> = (0..50)
        .map(|i| {
            let bf_path = bf_path.clone();
            let workspace = workspace.clone();
            let bead_id = test_bead_id.clone();
            let assignee = format!("worker-{}", i);
            tokio::spawn(async move { run_bf_claim(&bf_path, &workspace, &bead_id, &assignee, i) })
        })
        .collect();

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let success_count = results.iter().filter(|r| r.is_ok()).count();
    let failure_count = results.iter().filter(|r| r.is_err()).count();
    let lock_error_count = results
        .iter()
        .filter(|r| {
            if let Err(e) = r {
                e.to_string().contains("locked") || e.to_string().contains("database")
            } else {
                false
            }
        })
        .count();

    println!(
        "Results: {} success, {} failure, {} lock/database errors",
        success_count, failure_count, lock_error_count
    );

    // Print details of failures
    for (i, result) in results.iter().enumerate() {
        if let Err(e) = result {
            println!("Task {} failed: {}", i, e);
            if e.to_string().contains("locked") {
                println!("  -> This is a lock error (SQLite contention)");
            }
        }
    }

    // Cleanup: release the bead
    println!("\n=== Cleanup: releasing test bead ===");
    let _ = Command::new(&bf_path)
        .args([
            "update",
            &test_bead_id,
            "--status",
            "open",
            "--assignee",
            "",
        ])
        .current_dir(&workspace)
        .output();

    Ok(())
}

fn create_test_bead(bf_path: &PathBuf, workspace: &PathBuf) -> Result<String, anyhow::Error> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros();
    let title = format!("Test bead for concurrency - {}", timestamp);
    let body = "This bead is used to test concurrent bf operations.";

    let output = Command::new(bf_path)
        .args([
            "create",
            "--title",
            &title,
            "--description",
            body,
            "--label",
            "test-concurrency",
        ])
        .current_dir(workspace)
        .output()
        .context("bf create failed")?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let bead_id = stdout.trim().to_string();

    if !output.status.success() || bead_id.is_empty() {
        anyhow::bail!(
            "Failed to create test bead: stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(bead_id)
}

fn run_bf_claim(
    bf_path: &PathBuf,
    workspace: &PathBuf,
    bead_id: &str,
    assignee: &str,
    task_id: usize,
) -> Result<String, anyhow::Error> {
    let output = Command::new(bf_path)
        .args(["claim", "--assignee", assignee, "--json"])
        .current_dir(workspace)
        .output()
        .with_context(|| format!("bf claim spawn failed (task {})", task_id))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        anyhow::bail!(
            "bf claim exited with code {} (task {}, assignee {})\nstderr: {}\nstdout: {}",
            code,
            task_id,
            assignee,
            stderr,
            stdout.chars().take(200).collect::<String>()
        );
    }

    Ok(stdout)
}
