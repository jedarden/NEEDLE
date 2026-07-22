//! Test to demonstrate that bf list stderr is being swallowed in error messages.
//!
//! Run with: cargo run --example test_bf_list_error_output

use anyhow::{anyhow, Context, Result};

/// Simulate the error chain that occurs when bf list fails.
fn simulate_bf_list_error() -> Result<()> {
    // This simulates the chain in run_bf_in():
    // bail!("bf {args:?} exited with code {code}\nstderr: {stderr}\nstdout: {stdout}");
    let bf_stderr = "Error: database is locked\nsqlite error: 5";
    let bf_stdout = "";
    let code = 1;
    let args = ["list", "--json", "--limit", "0"];

    Err(anyhow!(
        "bf {args:?} exited with code {code}\nstderr: {bf_stderr}\nstdout: {bf_stdout}"
    ))
    .context("bf list failed")
}

/// Simulate how the error gets wrapped in StrandError.
/// This matches the real code pattern: `.map_err(|e| StrandError::StoreError(e.into()))`
fn simulate_strand_error() -> Result<()> {
    simulate_bf_list_error().map_err(|e| {
        // This matches the real pattern: StrandError::StoreError(e.into())
        // which preserves the error chain.
        let strand_err = needle::types::StrandError::StoreError(e);
        anyhow::Error::from(strand_err)
    })
}

fn main() {
    let error = simulate_strand_error().unwrap_err();

    println!("=== Display format (%e) - what currently gets logged ===");
    println!("{}", error);
    println!();

    println!("=== Debug alternate (%#?) - shows chain but with formatting ===");
    println!("{:#?}", error);
    println!();

    println!("=== Chain iter - what we need to log ===");
    for (i, cause) in error.chain().enumerate() {
        println!("Cause {}: {}", i, cause);
    }
}
