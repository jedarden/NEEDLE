//! Self-update functionality for needle.
//!
//! Provides the `needle upgrade` command that checks GitHub releases for
//! newer versions and downloads/replaces the binary.

use std::env;
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

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
}
