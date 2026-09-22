//! Startup diagnostics for workspaces that still use bead-forge's legacy store.
//!
//! NEEDLE no longer drives the bead-forge command dialect, but an old
//! `.beads/issues.jsonl` store can still be encountered during migration. ADR-001
//! requires those workspaces to receive an actionable warning when the installed
//! `bf` version is known to have the historical `--limit 0` bug.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Versions of bead-forge with known compatibility problems.
const KNOWN_INCOMPATIBLE_VERSIONS: &[(&str, &str)] = &[
    (
        "0.2.0",
        "--limit 0 returns empty set (should return all beads)",
    ),
    (
        "0.1.",
        "pre-0.2.0 releases can truncate results when the default limit is used",
    ),
];

const VERSION_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of the bead-forge compatibility handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionCheck {
    /// The response is present and does not match a known incompatibility.
    Ok,
    /// A known-incompatible version was reported.
    KnownBad {
        version: String,
        issues: Vec<String>,
    },
    /// The handshake could not be completed.
    Failed { reason: String },
}

/// Run `bf --version` and classify its response against known incompatibilities.
pub async fn check_bead_forge_version(bf_path: &Path) -> VersionCheck {
    let path = bf_path.to_owned();
    let mut command = tokio::process::Command::new(path);
    command.arg("--version").kill_on_drop(true);
    let result = tokio::time::timeout(VERSION_CHECK_TIMEOUT, command.output()).await;

    match result {
        Ok(Ok(output)) => classify_version_output(output),
        Ok(Err(error)) => VersionCheck::Failed {
            reason: format!("failed to spawn bf --version: {error}"),
        },
        Err(_) => VersionCheck::Failed {
            reason: format!(
                "bf --version timed out after {}s",
                VERSION_CHECK_TIMEOUT.as_secs()
            ),
        },
    }
}

/// Emit the ADR-001 startup warning for a bead-forge version response.
pub async fn run_version_handshake(bf_path: &Path) {
    match check_bead_forge_version(bf_path).await {
        VersionCheck::Ok => {
            tracing::debug!(binary = %bf_path.display(), "bead-forge version compatibility check passed");
        }
        VersionCheck::KnownBad { version, issues } => {
            for issue in issues {
                tracing::warn!(
                    binary = %bf_path.display(),
                    version = %version,
                    issue = %issue,
                    "incompatible bead-forge version detected; upgrade bead-forge or migrate this legacy workspace to bead-rs"
                );
            }
        }
        VersionCheck::Failed { reason } => {
            tracing::warn!(
                binary = %bf_path.display(),
                reason = %reason,
                "could not complete bead-forge version compatibility check for legacy workspace"
            );
        }
    }
}

/// Run the handshake from the synchronous store-open path.
fn run_version_handshake_sync(bf_path: &Path) {
    match run_sync_version_check(bf_path) {
        VersionCheck::Ok => {
            tracing::debug!(binary = %bf_path.display(), "bead-forge version compatibility check passed");
        }
        VersionCheck::KnownBad { version, issues } => {
            for issue in issues {
                tracing::warn!(
                    binary = %bf_path.display(),
                    version = %version,
                    issue = %issue,
                    "incompatible bead-forge version detected; upgrade bead-forge or migrate this legacy workspace to bead-rs"
                );
            }
        }
        VersionCheck::Failed { reason } => {
            tracing::warn!(
                binary = %bf_path.display(),
                reason = %reason,
                "could not complete bead-forge version compatibility check for legacy workspace"
            );
        }
    }
}

/// Warn when store startup is pointed at a legacy bead-forge workspace.
pub(crate) fn warn_for_legacy_workspace(workspace: &Path, configured_path: Option<&Path>) {
    if !workspace.join(".beads/issues.jsonl").is_file() {
        return;
    }

    let Some(bf_path) = configured_path
        .filter(|path| path.file_name().and_then(|name| name.to_str()) == Some("bf"))
        .map(Path::to_owned)
        .or_else(find_bead_forge_binary)
    else {
        tracing::warn!(
            workspace = %workspace.display(),
            "legacy bead-forge workspace detected, but no bf executable was found for the version compatibility handshake; migrate this workspace to bead-rs"
        );
        return;
    };

    run_version_handshake_sync(&bf_path);
}

fn classify_version_output(output: Output) -> VersionCheck {
    if !output.status.success() {
        return VersionCheck::Failed {
            reason: format!(
                "bf --version exited with code {}",
                output.status.code().unwrap_or(-1)
            ),
        };
    }

    // bf has historically written version text to stdout. Include stderr as a
    // fallback because wrappers often forward their version response there.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let version = if stdout.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };

    if version.is_empty() {
        return VersionCheck::Failed {
            reason: "bf --version produced no output".to_string(),
        };
    }

    let issues = KNOWN_INCOMPATIBLE_VERSIONS
        .iter()
        .filter(|(prefix, _)| {
            version.starts_with(prefix)
                || version
                    .split_whitespace()
                    .any(|token| token.starts_with(prefix))
        })
        .map(|(_, issue)| (*issue).to_string())
        .collect::<Vec<_>>();

    if issues.is_empty() {
        VersionCheck::Ok
    } else {
        VersionCheck::KnownBad {
            version: version.to_string(),
            issues,
        }
    }
}

fn run_sync_version_check(bf_path: &Path) -> VersionCheck {
    let mut child = match Command::new(bf_path)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return VersionCheck::Failed {
                reason: format!("failed to spawn bf --version: {error}"),
            };
        }
    };

    let deadline = Instant::now() + VERSION_CHECK_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return VersionCheck::Failed {
                    reason: format!(
                        "bf --version timed out after {}s",
                        VERSION_CHECK_TIMEOUT.as_secs()
                    ),
                };
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return VersionCheck::Failed {
                    reason: format!("failed waiting for bf --version: {error}"),
                };
            }
        }
    }

    match child.wait_with_output() {
        Ok(output) => classify_version_output(output),
        Err(error) => VersionCheck::Failed {
            reason: format!("failed to collect bf --version output: {error}"),
        },
    }
}

fn find_bead_forge_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        if let Some(candidate) = std::env::split_paths(&path)
            .map(|directory| directory.join("bf"))
            .find(|candidate| is_executable(candidate))
        {
            return Some(candidate);
        }
    }

    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/bin/bf"))
        .filter(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
