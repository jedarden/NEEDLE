//! Startup diagnostics for backend versions known to misbehave.
//!
//! Two handshakes run at store open, one per store shape:
//!
//! - A workspace still on bead-forge's legacy `.beads/issues.jsonl` store is
//!   probed with `bf --version` and warned about when the installed `bf` is
//!   known to have the historical `--limit 0` bug (`warn_for_legacy_workspace`).
//!   NEEDLE no longer drives the bead-forge command dialect, but such stores
//!   are still encountered during migration.
//! - A bead-rs store's running binary has already answered the descriptor's
//!   `version_command` by the time the identity check passes; that output is
//!   classified against the version ranges the descriptor itself declares
//!   incompatible — its version-scoped quirks (`warn_for_backend_quirks`).
//!
//! Both serve ADR-001 axis 3's second half: an incompatible backend version
//! is named at startup instead of silently relying on the workarounds that
//! keep its defects harmless.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::bead_store::backend::{BackendVersion, BeadBackend};

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

/// Classify a backend's `version_command` output against the version ranges
/// its own descriptor declares incompatible (its version-scoped quirks,
/// ADR-013 §4) and warn once per applying quirk.
///
/// This is the bead-rs half of ADR-001 axis 3's version handshake. The legacy
/// bead-forge handshake above has to spawn its own probe because legacy
/// stores are not opened through a descriptor; a bead-rs store open has
/// already run the descriptor's `version_command` for the identity check
/// (`verify_backend_identity`), so that output is classified here rather
/// than probed a second time. The declaration lives in the descriptor — not
/// in a second table here — so narrowing a quirk's range retires both the
/// workaround and the warning with one edit.
///
/// Two shapes stay silent: output without a parseable `major.minor.patch`
/// triple cannot be classified (the identity check has already proven the
/// binary is the expected one, and the quirk workarounds stay in effect
/// either way), and a quirk without a `version_requirement` is a permanent
/// part of the store contract rather than a defect an upgrade can retire —
/// warning about it on every store open would be noise, not signal.
pub(crate) fn warn_for_backend_quirks(descriptor: &BeadBackend, version_output: &str) {
    let Some(version) = BackendVersion::parse(version_output) else {
        tracing::debug!(
            backend = %descriptor.name,
            version_output = %version_output.trim(),
            "backend version output carried no parseable major.minor.patch triple; skipping known-defect classification"
        );
        return;
    };

    for quirk in descriptor
        .quirks
        .iter()
        .filter(|quirk| quirk.version_requirement.is_some())
        .filter(|quirk| quirk.applies_to(Some(version)))
    {
        tracing::warn!(
            backend = %descriptor.name,
            version = %version,
            declared_range = quirk.version_requirement.as_deref().unwrap_or_default(),
            quirk = %quirk.name,
            "incompatible backend version detected at startup; the descriptor's workaround stays in effect — upgrade past the declared range and narrow the quirk to retire the warning"
        );
        // The quirk description is the retirement contract — the why, the
        // observed versions, and the evidence that narrows the range. It is
        // a paragraph, so it rides at debug to keep the warning one line.
        tracing::debug!(
            backend = %descriptor.name,
            quirk = %quirk.name,
            description = %quirk.description,
            "quirk retirement contract"
        );
    }
}

/// [`warn_for_backend_quirks`], at most once per process.
///
/// The condition depends only on the installed backend binary, so every
/// store open in one process classifies the same version; without the
/// guard a worker that reopens a workspace every Explore cycle would emit
/// the identical warning each cycle for the lifetime of the process. The
/// unguarded function stays separate so tests can assert on it directly.
pub(crate) fn warn_for_backend_quirks_once(descriptor: &BeadBackend, version_output: &str) {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let version = BackendVersion::parse(version_output);
    let key = format!(
        "{}:{}",
        descriptor.name,
        version
            .map(|version| version.to_string())
            .unwrap_or_else(|| version_output.trim().to_string())
    );
    let seen = WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    let should_warn = seen
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key);
    if should_warn {
        warn_for_backend_quirks(descriptor, version_output);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture `tracing` output written through the default subscriber.
    ///
    /// Same shape as the log-capture helpers used by the `hoop_hooks` and
    /// `health` modules: a `MakeWriter` over a shared byte buffer, so the
    /// store-open warning path can be asserted on without owning stderr.
    #[derive(Clone, Default)]
    struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    struct CapturedLogWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = CapturedLogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CapturedLogWriter(self.0.clone())
        }
    }

    fn captured_logs_subscriber(captured: &CapturedLogs) -> impl tracing::Subscriber {
        tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .without_time()
            .finish()
    }

    fn captured_text(captured: &CapturedLogs) -> String {
        String::from_utf8(captured.0.lock().unwrap().clone()).unwrap()
    }

    /// A legacy bead-forge store: the flat `issues.jsonl` layout that predates
    /// the bead-rs migration. `warn_for_legacy_workspace` keys on exactly this
    /// file, and every production store open passes through it
    /// (`open_configured` → Explore's `DefaultStoreFactory`), so a store like
    /// this is what the Explore strand hands its scan.
    fn legacy_workspace(root: &Path) -> PathBuf {
        let workspace = root.join("ws");
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();
        std::fs::write(
            workspace.join(".beads/issues.jsonl"),
            "{\"id\":\"legacy-1\",\"status\":\"open\"}\n",
        )
        .unwrap();
        workspace
    }

    /// A `bf`-named executable whose `--version` prints `version_output`.
    #[cfg(unix)]
    fn fake_bf(root: &Path, version_output: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("bf");
        std::fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' '{version_output}'\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// ADR-001 axis 3, emission half: a legacy workspace whose `bf` reports a
    /// known-incompatible version must produce a WARN that identifies the
    /// version, the concrete incompatibility, and the remediation — not fail
    /// silently into the truncation the version is known for.
    #[cfg(unix)]
    #[test]
    fn legacy_workspace_with_known_bad_bf_warns_actionably() {
        let captured = CapturedLogs::default();
        let dir = tempfile::tempdir().unwrap();
        let workspace = legacy_workspace(dir.path());
        let bf = fake_bf(dir.path(), "bf 0.2.0");

        tracing::subscriber::with_default(captured_logs_subscriber(&captured), || {
            warn_for_legacy_workspace(&workspace, Some(&bf));
        });

        let logs = captured_text(&captured);
        assert!(logs.contains("WARN"), "expected a warning, got: {logs}");
        assert!(
            logs.contains("incompatible bead-forge version detected"),
            "warning must name the condition, got: {logs}"
        );
        assert!(
            logs.contains("0.2.0"),
            "warning must identify the offending version, got: {logs}"
        );
        assert!(
            logs.contains("--limit 0 returns empty set"),
            "warning must state the concrete incompatibility, got: {logs}"
        );
        assert!(
            logs.contains("upgrade bead-forge or migrate this legacy workspace to bead-rs"),
            "warning must carry the remediation, got: {logs}"
        );
    }

    /// The second known-incompatible family: pre-0.2.0 releases are matched by
    /// the `"0.1."` prefix (the version arrives embedded in `bf 0.1.7`, not as
    /// the whole first line), and their issue is the default-limit truncation —
    /// the exact failure ADR-001's explicit-limits rule exists to prevent.
    #[cfg(unix)]
    #[test]
    fn legacy_workspace_with_pre_02_bf_warns_default_limit_truncation() {
        let captured = CapturedLogs::default();
        let dir = tempfile::tempdir().unwrap();
        let workspace = legacy_workspace(dir.path());
        let bf = fake_bf(dir.path(), "bf 0.1.7");

        tracing::subscriber::with_default(captured_logs_subscriber(&captured), || {
            warn_for_legacy_workspace(&workspace, Some(&bf));
        });

        let logs = captured_text(&captured);
        assert!(logs.contains("WARN"), "expected a warning, got: {logs}");
        assert!(
            logs.contains("0.1.7"),
            "warning must identify the offending version, got: {logs}"
        );
        assert!(
            logs.contains("pre-0.2.0 releases can truncate results"),
            "warning must state the truncation incompatibility, got: {logs}"
        );
    }

    /// The quiet half of the same contract: a compatible `bf` version must not
    /// warn, so a healthy legacy workspace opens without log noise.
    #[cfg(unix)]
    #[test]
    fn legacy_workspace_with_compatible_bf_does_not_warn() {
        let captured = CapturedLogs::default();
        let dir = tempfile::tempdir().unwrap();
        let workspace = legacy_workspace(dir.path());
        let bf = fake_bf(dir.path(), "bf 0.3.0");

        tracing::subscriber::with_default(captured_logs_subscriber(&captured), || {
            warn_for_legacy_workspace(&workspace, Some(&bf));
        });

        let logs = captured_text(&captured);
        assert!(
            !logs.contains("WARN"),
            "a compatible version must not warn, got: {logs}"
        );
        assert!(
            !logs.contains("incompatible"),
            "a compatible version must not be reported as incompatible, got: {logs}"
        );
    }

    /// A legacy workspace with no `bf` anywhere must still warn — the
    /// migration guidance is the actionable signal, and silence here would
    /// hide an unverifiable handshake behind a missing binary. Runs under
    /// [`crate::util::test_env::isolate_env`] because the binary search reads
    /// the process `PATH` and `HOME`.
    #[test]
    fn legacy_workspace_without_bf_binary_warns_migration_guidance() {
        let _env = crate::util::test_env::isolate_env();
        let empty = tempfile::tempdir().unwrap();
        std::env::set_var("PATH", empty.path());
        std::env::set_var("HOME", empty.path());

        let captured = CapturedLogs::default();
        let dir = tempfile::tempdir().unwrap();
        let workspace = legacy_workspace(dir.path());

        tracing::subscriber::with_default(captured_logs_subscriber(&captured), || {
            warn_for_legacy_workspace(&workspace, None);
        });

        let logs = captured_text(&captured);
        assert!(logs.contains("WARN"), "expected a warning, got: {logs}");
        assert!(
            logs.contains("no bf executable was found"),
            "warning must say the handshake could not run, got: {logs}"
        );
        assert!(
            logs.contains("migrate this workspace to bead-rs"),
            "warning must carry the remediation, got: {logs}"
        );
    }

    /// A workspace without the legacy store layout must never trigger the
    /// handshake at all — the pointing-at-a-legacy-workspace warning (and any
    /// spawn noise from a dead binary) belongs only to stores that need it.
    /// The configured `bf` path deliberately does not exist: if the handshake
    /// ran, the failed spawn would warn and fail this test.
    #[test]
    fn workspace_without_legacy_store_stays_silent() {
        let captured = CapturedLogs::default();
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("modern-ws");
        std::fs::create_dir_all(&workspace).unwrap();

        tracing::subscriber::with_default(captured_logs_subscriber(&captured), || {
            warn_for_legacy_workspace(&workspace, Some(&dir.path().join("bf")));
        });

        let logs = captured_text(&captured);
        assert!(
            !logs.contains("WARN"),
            "a non-legacy workspace must not warn, got: {logs}"
        );
    }

    fn bead_rs_descriptor() -> crate::bead_store::backend::BeadBackend {
        crate::bead_store::builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .expect("the built-in bead-rs descriptor must exist")
    }

    /// ADR-001 axis 3, bead-rs emission half: the version output already
    /// collected by the startup identity probe must produce an actionable
    /// warning when it falls in a descriptor-declared incompatible range.
    #[test]
    fn bead_rs_known_bad_version_warns_actionably() {
        let captured = CapturedLogs::default();
        let descriptor = bead_rs_descriptor();

        tracing::subscriber::with_default(captured_logs_subscriber(&captured), || {
            warn_for_backend_quirks(&descriptor, "bead 0.2.6 (commit test)");
        });

        let logs = captured_text(&captured);
        assert!(logs.contains("WARN"), "expected a warning, got: {logs}");
        assert!(
            logs.contains("incompatible backend version detected at startup"),
            "warning must identify the condition, got: {logs}"
        );
        assert!(
            logs.contains("0.2.6"),
            "warning must identify the offending version, got: {logs}"
        );
        assert!(
            logs.contains("limit_zero_returns_empty_set"),
            "warning must identify the active compatibility quirk, got: {logs}"
        );
        assert!(
            logs.contains("upgrade past the declared range"),
            "warning must carry remediation guidance, got: {logs}"
        );
    }

    /// A version outside the declared defect range must not be reported as
    /// incompatible; the descriptor's quirk range is the compatibility source
    /// of truth rather than a permanent warning for every startup.
    #[test]
    fn bead_rs_supported_version_stays_quiet() {
        let captured = CapturedLogs::default();
        let descriptor = bead_rs_descriptor();

        tracing::subscriber::with_default(captured_logs_subscriber(&captured), || {
            warn_for_backend_quirks(&descriptor, "bead 0.3.0 (commit test)");
        });

        let logs = captured_text(&captured);
        assert!(
            !logs.contains("WARN"),
            "a supported version must not warn, got: {logs}"
        );
        assert!(
            !logs.contains("incompatible backend version"),
            "a supported version must not be reported as incompatible, got: {logs}"
        );
    }
}
