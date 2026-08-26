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

    // Whether the working tree had uncommitted changes at build time. A build
    // from a dirty tree does not correspond to any commit, so it must never be
    // mistaken for the release built from the same version.
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .map(|output| !String::from_utf8_lossy(&output.stdout).trim().is_empty())
        .unwrap_or(false);

    // The full string shown by `needle --version`.
    //
    // The semver alone cannot identify a build: Cargo.toml holds one version
    // for the whole development window between releases, so every binary built
    // in that window reports the same thing. On 2026-08-26 three different
    // needle binaries on one host all reported "needle 0.5.0" -- an Aug 15
    // build, an Aug 21 build, and the actual v0.5.0 release -- and the fleet
    // was running the oldest of them. Carrying the commit and build date makes
    // that difference visible instead of requiring a checksum to detect.
    let version_string = format!(
        "{} ({}{} {})",
        env!("CARGO_PKG_VERSION"),
        commit_sha,
        if dirty { "-dirty" } else { "" },
        build_timestamp
    );

    // Set cargo environment variables that will be available at compile time
    println!("cargo:rustc-env=NEEDLE_COMMIT_SHA={}", commit_sha);
    println!("cargo:rustc-env=NEEDLE_BUILD_TIMESTAMP={}", build_timestamp);
    println!("cargo:rustc-env=NEEDLE_BUILD_DIRTY={}", dirty);
    println!("cargo:rustc-env=NEEDLE_VERSION_STRING={}", version_string);

    // Rebuild if git HEAD changes, or if the working tree's staged state does
    // (so the -dirty marker cannot go stale against a rebuilt binary).
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-env-var=NEEDLE_COMMIT_SHA");
}
