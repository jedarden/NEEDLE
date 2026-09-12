//! Build script for NEEDLE.
//!
//! Embeds git commit SHA and build timestamp into the binary at compile time.

use std::process::Command;

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn command_stdout(command: &mut Command) -> Option<String> {
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn git_stdout(args: &[&str]) -> Option<String> {
    command_stdout(Command::new("git").args(args))
}

fn format_epoch(epoch: &str) -> Option<String> {
    command_stdout(Command::new("date").args([
        "-u",
        "-d",
        &format!("@{epoch}"),
        "+%Y-%m-%dT%H:%M:%SZ",
    ]))
}

fn reproducible_timestamp() -> String {
    // Reproducible-build callers own SOURCE_DATE_EPOCH. For ordinary builds,
    // use the immutable commit time instead of the time this script happened
    // to run. Exported source archives without either value degrade visibly
    // rather than reintroducing wall-clock nondeterminism.
    nonempty_env("SOURCE_DATE_EPOCH")
        .or_else(|| git_stdout(&["show", "-s", "--format=%ct", "HEAD"]))
        .and_then(|epoch| format_epoch(&epoch))
        .unwrap_or_else(|| "unknown".to_string())
}

fn main() {
    // Source archives can inject their revision; git checkouts derive it.
    let commit_sha = nonempty_env("NEEDLE_COMMIT_SHA")
        .or_else(|| git_stdout(&["rev-parse", "--short", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_string());

    let build_timestamp = reproducible_timestamp();

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
    println!("cargo:rerun-if-env-changed=NEEDLE_COMMIT_SHA");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}
