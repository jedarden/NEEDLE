//! Test to demonstrate the ACTUAL current bf list error format (2026-07-15).
//!
//! Run with: cargo run --example test_actual_bf_error_format

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

// Import StrandError from the crate
use needle::types::StrandError;

/// Simulate the ACTUAL current error chain from run_bf() in bead_store/mod.rs (2026-07-15).
/// This matches lines 1030-1035:
///   let base_error = anyhow::anyhow!("bf {args:?} exited with code {code}");
///   let error_with_stderr = if stderr.is_empty() {
///       base_error
///   } else {
///       base_error.context(format!("bf stderr: {}", stderr.trim()))
///   };
///   return Err(error_with_stderr);
fn simulate_actual_bf_error() -> Result<()> {
    let args = ["list", "--json", "--limit", "999999"];
    let code = 1;
    let stderr = "Error: database is locked\nsqlite error: 5";

    // This is the ACTUAL current structure
    let base_error = anyhow!("bf {args:?} exited with code {code}");
    let error_with_stderr = if !stderr.is_empty() {
        base_error.context(format!("bf stderr: {}", stderr.trim()))
    } else {
        base_error
    };

    Err(error_with_stderr)
}

/// Simulate how the error gets wrapped in StrandError.
/// This matches the real code pattern: `.map_err(|e| StrandError::StoreError(e.into()))`
fn simulate_strand_error() -> Result<()> {
    simulate_actual_bf_error().map_err(|e| {
        // This matches the real pattern: StrandError::StoreError(e.into())
        // which preserves the error chain.
        let strand_err = needle::types::StrandError::StoreError(e.into());
        anyhow::Error::from(strand_err)
    })
}

fn main() {
    let error = simulate_strand_error().unwrap_err();

    println!("=== Display format (%e) - ACTUAL current output ===");
    println!("{}", error);
    println!();

    println!("=== Debug alternate (%#?) - shows chain ===");
    println!("{:#?}", error);
    println!();

    println!("=== Chain iter - raw error chain ===");
    for (i, cause) in error.chain().enumerate() {
        println!("Cause {}: {}", i, cause);
    }
}
