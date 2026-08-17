//! Build script for NEEDLE.
//!
//! Embeds git commit SHA and build timestamp into the binary at compile time.

use std::process::Command;

fn main() {
    // Get the current git commit SHA (short form)
    let commit_sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .map(|output| {
            let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if sha.is_empty() {
                "unknown".to_string()
            } else {
                sha
            }
        })
        .unwrap_or_else(|_| "unknown".to_string());

    // Get the build timestamp
    let build_timestamp = Command::new("date")
        .arg("-u")
        .arg("+%Y-%m-%dT%H:%M:%SZ")
        .output()
        .map(|output| {
            let timestamp = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if timestamp.is_empty() {
                "unknown".to_string()
            } else {
                timestamp
            }
        })
        .unwrap_or_else(|_| "unknown".to_string());

    // Set cargo environment variables that will be available at compile time
    println!("cargo:rustc-env=NEEDLE_COMMIT_SHA={}", commit_sha);
    println!("cargo:rustc-env=NEEDLE_BUILD_TIMESTAMP={}", build_timestamp);

    // Rebuild if git HEAD changes
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-env-var=NEEDLE_COMMIT_SHA");
}
