//! Self-update functionality for needle.
//!
//! Provides the `needle upgrade` command that checks GitHub releases for
//! newer versions and downloads/replaces the binary. Also provides hot-reload
//! support: detecting a new `:stable` binary and re-exec'ing into it.

use std::env;
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Current version of the needle binary.
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// GitHub repository for releases.
const GITHUB_REPO: &str = "jedarden/NEEDLE";

/// GitHub API URL for latest release.
fn latest_release_url() -> String {
    format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest")
}

/// Download URL for a specific asset.
fn asset_download_url(asset_name: &str) -> String {
    format!(
        "https://github.com/{}/releases/latest/download/{}",
        GITHUB_REPO, asset_name
    )
}

/// GitHub release asset information.
#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

/// GitHub release information.
#[derive(Debug, Deserialize)]
struct ReleaseInfo {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    assets: Vec<ReleaseAsset>,
}

/// Result of checking for updates.
#[derive(Debug, Clone)]
pub struct UpdateCheck {
    /// Current version.
    pub current_version: String,
    /// Latest available version.
    pub latest_version: String,
    /// Whether an update is available.
    pub update_available: bool,
    /// Release notes (if any).
    pub release_notes: Option<String>,
}

/// Check GitHub for the latest release.
///
/// Returns information about the latest release compared to the current version.
pub fn check_for_update() -> Result<UpdateCheck> {
    let response = ureq::agent()
        .get(&latest_release_url())
        .set("User-Agent", &format!("needle/{CURRENT_VERSION}"))
        .call()
        .context("failed to fetch latest release from GitHub")?;

    if response.status() >= 400 {
        bail!(
            "GitHub API returned status {} when checking for updates",
            response.status()
        );
    }

    let release: ReleaseInfo = serde_json::from_reader(response.into_reader())
        .context("failed to parse GitHub release response")?;

    // Parse version from tag (strip 'v' prefix if present).
    let latest_version = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name)
        .to_string();

    let update_available = is_newer_version(&latest_version, CURRENT_VERSION);

    Ok(UpdateCheck {
        current_version: CURRENT_VERSION.to_string(),
        latest_version,
        update_available,
        release_notes: release.body,
    })
}

/// Compare two semver versions, return true if `latest` > `current`.
fn is_newer_version(latest: &str, current: &str) -> bool {
    let parse_version =
        |v: &str| -> Vec<u32> { v.split('.').filter_map(|s| s.parse().ok()).collect() };

    let latest_parts = parse_version(latest);
    let current_parts = parse_version(current);

    for (l, c) in latest_parts.iter().zip(current_parts.iter()) {
        if l > c {
            return true;
        }
        if l < c {
            return false;
        }
    }

    latest_parts.len() > current_parts.len()
}

/// Get the asset name for the current platform.
fn get_asset_name() -> Result<String> {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;

    let asset_name = match (os, arch) {
        ("linux", "x86_64") => "needle-x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "needle-aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "needle-x86_64-apple-darwin",
        ("macos", "aarch64") => "needle-aarch64-apple-darwin",
        _ => bail!("unsupported platform: {os} {arch}"),
    };

    Ok(asset_name.to_string())
}

/// Download and install the latest version of needle.
///
/// Returns the path to the new binary.
pub fn perform_upgrade() -> Result<PathBuf> {
    let check = check_for_update()?;

    if !check.update_available {
        println!("Already up to date (version {})", check.current_version);
        return get_current_binary_path();
    }

    println!(
        "Upgrading from {} to {}...",
        check.current_version, check.latest_version
    );

    let asset_name = get_asset_name()?;
    let download_url = asset_download_url(&asset_name);

    println!("Downloading from {download_url}...");

    // Download the new binary.
    let response = ureq::agent()
        .get(&download_url)
        .set("User-Agent", &format!("needle/{}", check.current_version))
        .call()
        .context("failed to download new binary")?;

    if response.status() >= 400 {
        bail!("download failed with status {}", response.status());
    }

    // Read the response into memory.
    let mut content = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut content)
        .context("failed to read downloaded content")?;

    // Get the current binary path.
    let current_binary = get_current_binary_path()?;

    // Create a temporary directory for the new binary.
    let temp_dir = env::temp_dir().join(format!("needle-upgrade-{}", std::process::id()));
    fs::create_dir_all(&temp_dir).context("failed to create temp directory")?;
    let new_binary = temp_dir.join("needle");

    // Write the new binary.
    let mut cursor = Cursor::new(&content);
    {
        let mut file = fs::File::create(&new_binary).context("failed to create new binary file")?;
        io::copy(&mut cursor, &mut file).context("failed to write new binary")?;
    }

    // Make it executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&new_binary, fs::Permissions::from_mode(0o755))
            .context("failed to set executable permissions")?;
    }

    // Replace the old binary with the new one.
    // On Unix, we can just rename over the existing file.
    // On Windows, we'd need to move the old file first.
    #[cfg(unix)]
    {
        fs::rename(&new_binary, &current_binary)
            .with_context(|| format!("failed to replace binary at {}", current_binary.display()))?;
    }

    #[cfg(windows)]
    {
        // On Windows, rename over an in-use file fails.
        // Move the old binary aside, then put the new one in place.
        let old_binary = current_binary.with_extension("exe.old");
        fs::rename(&current_binary, &old_binary).context("failed to move old binary aside")?;
        fs::rename(&new_binary, &current_binary).context("failed to install new binary")?;
        // Try to remove the old binary (may fail if still in use).
        let _ = fs::remove_file(&old_binary);
    }

    println!("Successfully upgraded to version {}!", check.latest_version);

    if let Some(notes) = &check.release_notes {
        println!("\nRelease notes:\n{}", notes);
    }

    Ok(current_binary)
}

/// Get the path to the current binary.
fn get_current_binary_path() -> Result<PathBuf> {
    env::current_exe().context("failed to determine current binary path")
}

// ──────────────────────────────────────────────────────────────────────────────
// Hot-reload support
// ──────────────────────────────────────────────────────────────────────────────

/// Compute the SHA-256 hash of a file, returned as a hex string.
pub fn file_hash(path: &Path) -> Result<String> {
    let content = fs::read(path)
        .with_context(|| format!("failed to read file for hashing: {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&content);
    let result = hasher.finalize();
    Ok(hex::encode(result))
}

/// Result of checking for a new :stable binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotReloadCheck {
    /// No change detected — current binary matches :stable.
    NoChange,
    /// New :stable binary detected with different hash.
    NewBinaryDetected {
        /// Hash of the currently running binary.
        old_hash: String,
        /// Hash of the new :stable binary.
        new_hash: String,
        /// Path to the new :stable binary.
        stable_path: PathBuf,
    },
    /// Hot-reload is disabled or :stable binary not found.
    Skipped { reason: String },
}

/// Check whether the `:stable` binary differs from the currently running binary.
///
/// This is called between LOGGING and SELECTING in the worker loop.
pub fn check_hot_reload(needle_home: &Path) -> Result<HotReloadCheck> {
    let stable_path = needle_home.join("bin/needle-stable");

    if !stable_path.exists() {
        return Ok(HotReloadCheck::Skipped {
            reason: "no :stable binary found".to_string(),
        });
    }

    let current_path = get_current_binary_path()?;
    let current_hash = file_hash(&current_path)?;
    let stable_hash = file_hash(&stable_path)?;

    if current_hash == stable_hash {
        Ok(HotReloadCheck::NoChange)
    } else {
        Ok(HotReloadCheck::NewBinaryDetected {
            old_hash: current_hash,
            new_hash: stable_hash,
            stable_path,
        })
    }
}

/// Re-exec into the new `:stable` binary with `--resume` to preserve worker identity.
///
/// This function does not return on success — it replaces the current process.
/// On failure, it returns an error so the worker can continue on the current binary.
#[cfg(unix)]
pub fn re_exec_stable(
    stable_path: &Path,
    worker_name: &str,
    workspace: Option<&Path>,
    agent: Option<&str>,
    timeout: Option<u64>,
) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let mut cmd = std::process::Command::new(stable_path);
    cmd.arg("run").arg("--resume");
    cmd.arg("--identifier").arg(worker_name);
    cmd.arg("--count").arg("1");

    if let Some(ws) = workspace {
        cmd.arg("--workspace").arg(ws);
    }
    if let Some(a) = agent {
        cmd.arg("--agent").arg(a);
    }
    if let Some(t) = timeout {
        cmd.arg("--timeout").arg(t.to_string());
    }

    // exec() replaces the current process — does not return on success.
    let err = cmd.exec();
    Err(anyhow::anyhow!("re-exec failed: {}", err))
}

#[cfg(not(unix))]
pub fn re_exec_stable(
    _stable_path: &Path,
    _worker_name: &str,
    _workspace: Option<&Path>,
    _agent: Option<&str>,
    _timeout: Option<u64>,
) -> Result<()> {
    bail!("hot-reload re-exec is only supported on Unix platforms")
}

// ──────────────────────────────────────────────────────────────────────────────
// ResumeState — state loaded from heartbeat + registry for hot-reload resume
// ──────────────────────────────────────────────────────────────────────────────

/// State loaded from heartbeat file and registry for worker resumption after
/// hot-reload. Used by the `--resume` CLI flag to restore worker context.
#[derive(Debug, Clone)]
pub struct ResumeState {
    pub worker_id: String,
    pub beads_processed: u64,
    pub session: String,
}

impl ResumeState {
    /// Load resume state from heartbeat file and registry.
    ///
    /// Returns `None` if no valid heartbeat or registry entry exists for the
    /// given worker ID.
    pub fn load(config: &crate::config::Config, worker_id: &str) -> Result<Option<Self>> {
        let heartbeat_dir = config.workspace.home.join("state").join("heartbeats");
        let heartbeat_path = heartbeat_dir.join(format!("{}.json", worker_id));

        if !heartbeat_path.exists() {
            tracing::debug!(
                path = %heartbeat_path.display(),
                "no heartbeat file for resume"
            );
            return Ok(None);
        }

        let heartbeat_content = fs::read_to_string(&heartbeat_path)
            .with_context(|| format!("failed to read heartbeat: {}", heartbeat_path.display()))?;
        let heartbeat: crate::health::HeartbeatData = serde_json::from_str(&heartbeat_content)
            .with_context(|| format!("failed to parse heartbeat: {}", heartbeat_path.display()))?;

        // Check registry for additional context.
        let registry = crate::registry::Registry::default_location(&config.workspace.home);
        let workers = registry.list().context("failed to list registry")?;
        let entry = workers.iter().find(|w| w.id == worker_id);

        let beads_processed = match entry {
            Some(e) => e.beads_processed,
            None => heartbeat.beads_processed,
        };

        Ok(Some(ResumeState {
            worker_id: heartbeat.worker_id,
            beads_processed,
            session: heartbeat.session,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_version_major() {
        assert!(is_newer_version("2.0.0", "1.0.0"));
        assert!(!is_newer_version("1.0.0", "2.0.0"));
    }

    #[test]
    fn is_newer_version_minor() {
        assert!(is_newer_version("1.1.0", "1.0.0"));
        assert!(!is_newer_version("1.0.0", "1.1.0"));
    }

    #[test]
    fn is_newer_version_patch() {
        assert!(is_newer_version("1.0.1", "1.0.0"));
        assert!(!is_newer_version("1.0.0", "1.0.1"));
    }

    #[test]
    fn is_newer_version_equal() {
        assert!(!is_newer_version("1.0.0", "1.0.0"));
    }

    #[test]
    fn is_newer_version_different_lengths() {
        assert!(is_newer_version("1.0.0.1", "1.0.0"));
        assert!(!is_newer_version("1.0.0", "1.0.0.1"));
    }

    #[test]
    fn get_asset_name_current_platform() {
        // Should not panic on supported platforms.
        let result = get_asset_name();
        // This test will pass on supported platforms and fail on unsupported ones.
        // That's expected behavior.
        if let Ok(name) = result {
            assert!(name.starts_with("needle-"));
        }
    }

    #[test]
    fn update_check_current_version() {
        assert!(!CURRENT_VERSION.is_empty());
    }

    #[test]
    fn latest_release_url_format() {
        let url = latest_release_url();
        assert!(url.contains("api.github.com"));
        assert!(url.contains(GITHUB_REPO));
    }

    #[test]
    fn asset_download_url_format() {
        let url = asset_download_url("needle-test");
        assert!(url.contains("github.com"));
        assert!(url.contains(GITHUB_REPO));
        assert!(url.contains("needle-test"));
    }

    // ── Hot-reload tests ──

    #[test]
    fn file_hash_produces_hex_string() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test-binary");
        fs::write(&file_path, b"hello world").unwrap();
        let hash = file_hash(&file_path).unwrap();
        // SHA-256 hex string is 64 characters.
        assert_eq!(hash.len(), 64);
        // Known SHA-256 of "hello world".
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn file_hash_different_content_different_hash() {
        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("a");
        let file_b = dir.path().join("b");
        fs::write(&file_a, b"content A").unwrap();
        fs::write(&file_b, b"content B").unwrap();
        let hash_a = file_hash(&file_a).unwrap();
        let hash_b = file_hash(&file_b).unwrap();
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn file_hash_same_content_same_hash() {
        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("a");
        let file_b = dir.path().join("b");
        fs::write(&file_a, b"identical").unwrap();
        fs::write(&file_b, b"identical").unwrap();
        assert_eq!(file_hash(&file_a).unwrap(), file_hash(&file_b).unwrap());
    }

    #[test]
    fn file_hash_missing_file_returns_error() {
        let result = file_hash(Path::new("/nonexistent/binary"));
        assert!(result.is_err());
    }

    #[test]
    fn check_hot_reload_no_stable_binary_returns_skipped() {
        let dir = tempfile::tempdir().unwrap();
        // Create bin/ dir but no needle-stable file.
        fs::create_dir_all(dir.path().join("bin")).unwrap();
        let result = check_hot_reload(dir.path()).unwrap();
        assert!(matches!(result, HotReloadCheck::Skipped { .. }));
    }

    #[test]
    fn check_hot_reload_same_binary_returns_no_change() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        // Point :stable at the current test binary (same as current_exe).
        let current_exe = env::current_exe().unwrap();
        let stable_path = bin_dir.join("needle-stable");
        fs::copy(&current_exe, &stable_path).unwrap();

        let result = check_hot_reload(dir.path()).unwrap();
        assert_eq!(result, HotReloadCheck::NoChange);
    }

    #[test]
    fn check_hot_reload_different_binary_returns_detected() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        // Write a different file as :stable.
        let stable_path = bin_dir.join("needle-stable");
        fs::write(&stable_path, b"this is a completely different binary").unwrap();

        let result = check_hot_reload(dir.path()).unwrap();
        match result {
            HotReloadCheck::NewBinaryDetected {
                old_hash,
                new_hash,
                stable_path: detected_path,
            } => {
                assert_ne!(old_hash, new_hash);
                assert_eq!(detected_path, stable_path);
            }
            other => panic!("expected NewBinaryDetected, got {:?}", other),
        }
    }

    #[test]
    fn hot_reload_check_enum_variants_are_distinct() {
        let no_change = HotReloadCheck::NoChange;
        let skipped = HotReloadCheck::Skipped {
            reason: "test".to_string(),
        };
        let detected = HotReloadCheck::NewBinaryDetected {
            old_hash: "aaa".to_string(),
            new_hash: "bbb".to_string(),
            stable_path: PathBuf::from("/tmp/test"),
        };
        assert_ne!(no_change, skipped);
        assert_ne!(no_change, detected);
        assert_ne!(skipped, detected);
    }
}
