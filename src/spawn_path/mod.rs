//! Spawn-path binary integrity guardrail.
//!
//! Detects when the spawn-path binary (the needle executable itself) is modified
//! in place without a corresponding hot-reload re-exec. This prevents silent
//! binary replacements that could introduce unexpected behavior or security issues.
//!
//! The guardrail works by:
//! 1. Recording binary metadata (SHA-256 hash, inode, mtime) at worker boot
//! 2. Checking the current binary state on subsequent operations
//! 3. Emitting a `spawn_path.modified_in_place` telemetry event when changes are detected
//! 4. Providing needle doctor warnings for affected workers

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Metadata recorded for the spawn-path binary at boot time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryMetadata {
    /// Path to the binary (from `std::env::current_exe()`).
    pub path: PathBuf,
    /// SHA-256 hash of the binary contents.
    pub hash: String,
    /// File inode (filesystem-specific identifier).
    pub inode: u64,
    /// Last modification time (seconds since Unix epoch).
    pub mtime_secs: i64,
    /// Size in bytes.
    pub size: u64,
}

impl BinaryMetadata {
    /// Record metadata for the current executable.
    pub fn from_current_exe() -> Result<Self> {
        let exe_path = std::env::current_exe()
            .context("failed to get current executable path")?;

        Self::from_path(&exe_path)
    }

    /// Record metadata for a specific binary path.
    pub fn from_path(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;

        let inode = metadata.ino();
        let mtime = metadata.modified()
            .with_context(|| format!("failed to get mtime for {}", path.display()))?;
        let mtime_secs = mtime.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let size = metadata.len();

        let hash = compute_sha256(path)?;

        Ok(Self {
            path: path.to_path_buf(),
            hash,
            inode,
            mtime_secs,
            size,
        })
    }

    /// Check if the binary at the recorded path has been modified in place.
    ///
    /// Returns `Ok(None)` if the binary is unchanged or doesn't exist.
    /// Returns `Ok(Some(modification))` if the binary was modified.
    pub fn detect_modification(&self) -> Result<Option<BinaryModification>> {
        // Check if binary still exists at the recorded path
        if !self.path.exists() {
            // Binary deleted — not an in-place modification, likely a re-exec in progress
            return Ok(None);
        }

        let current_metadata = Self::from_path(&self.path)?;

        // If any of the core attributes changed, it's an in-place modification
        if current_metadata.hash != self.hash {
            return Ok(Some(BinaryModification {
                path: self.path.clone(),
                original_hash: self.hash.clone(),
                current_hash: current_metadata.hash,
                original_inode: self.inode,
                current_inode: current_metadata.inode,
                original_mtime_secs: self.mtime_secs,
                current_mtime_secs: current_metadata.mtime_secs,
                original_size: self.size,
                current_size: current_metadata.size,
                modification_type: ModificationType::HashChanged,
            }));
        }

        // Hash unchanged, but inode or mtime changed — suspicious
        if current_metadata.inode != self.inode || current_metadata.mtime_secs != self.mtime_secs {
            return Ok(Some(BinaryModification {
                path: self.path.clone(),
                original_hash: self.hash.clone(),
                current_hash: current_metadata.hash,
                original_inode: self.inode,
                current_inode: current_metadata.inode,
                original_mtime_secs: self.mtime_secs,
                current_mtime_secs: current_metadata.mtime_secs,
                original_size: self.size,
                current_size: current_metadata.size,
                modification_type: ModificationType::MetadataChanged,
            }));
        }

        Ok(None)
    }
}

/// Description of a detected binary modification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryModification {
    /// Path to the modified binary.
    pub path: PathBuf,
    /// Original SHA-256 hash.
    pub original_hash: String,
    /// Current SHA-256 hash.
    pub current_hash: String,
    /// Original inode.
    pub original_inode: u64,
    /// Current inode.
    pub current_inode: u64,
    /// Original mtime (seconds since Unix epoch).
    pub original_mtime_secs: i64,
    /// Current mtime (seconds since Unix epoch).
    pub current_mtime_secs: i64,
    /// Original size in bytes.
    pub original_size: u64,
    /// Current size in bytes.
    pub current_size: u64,
    /// Type of modification detected.
    pub modification_type: ModificationType,
}

impl BinaryModification {
    /// Format a human-readable description of the modification.
    pub fn describe(&self) -> String {
        match self.modification_type {
            ModificationType::HashChanged => {
                format!(
                    "Binary at {} modified in place:\n  Hash changed: {} -> {}\n  Size: {} -> {} bytes\n  Inode: {} -> {}\n  Mtime: {} -> {}",
                    self.path.display(),
                    &self.original_hash[..16],
                    &self.current_hash[..16],
                    self.original_size,
                    self.current_size,
                    self.original_inode,
                    self.current_inode,
                    self.original_mtime_secs,
                    self.current_mtime_secs,
                )
            }
            ModificationType::MetadataChanged => {
                format!(
                    "Binary at {} has suspicious metadata changes (hash unchanged):\n  Inode: {} -> {}\n  Mtime: {} -> {}",
                    self.path.display(),
                    self.original_inode,
                    self.current_inode,
                    self.original_mtime_secs,
                    self.current_mtime_secs,
                )
            }
        }
    }
}

/// Type of modification detected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModificationType {
    /// Binary hash changed — definitive in-place modification.
    HashChanged,
    /// Inode or mtime changed but hash is the same — suspicious.
    MetadataChanged,
}

/// Compute SHA-256 hash of a file.
fn compute_sha256(path: &Path) -> Result<String> {
    let contents = fs::read(path)
        .with_context(|| format!("failed to read binary at {}", path.display()))?;

    let mut hasher = Sha256::new();
    hasher.update(&contents);
    let result = hasher.finalize();

    Ok(format!("{:x}", result))
}

/// Check the spawn-path binary and emit a telemetry event if modified.
///
/// This function is called during worker boot to detect if the binary
/// has been modified in place since the last recording. It takes:
///
/// - `recorded_metadata`: Optional previously recorded metadata (e.g., stored on disk)
/// - `emit_telemetry`: Function to emit the telemetry event
///
/// If `recorded_metadata` is None, this is the first boot and no check is performed.
/// The caller should record the returned metadata for future checks.
///
/// Returns the current binary metadata that should be persisted for future checks.
pub fn check_spawn_path_at_boot<F>(
    recorded_metadata: Option<BinaryMetadata>,
    mut emit_telemetry: F,
) -> Result<BinaryMetadata>
where
    F: FnMut(SpawnPathModificationEvent),
{
    let current_metadata = BinaryMetadata::from_current_exe()
        .context("failed to record current binary metadata")?;

    // If we have recorded metadata, check for modification
    if let Some(recorded) = recorded_metadata {
        // Only check if the path matches (worker is running the same binary)
        if recorded.path == current_metadata.path {
            if let Some(modification) = recorded.detect_modification()
                .context("failed to check for binary modification")?
            {
                emit_telemetry(SpawnPathModificationEvent {
                    path: modification.path.display().to_string(),
                    original_hash: modification.original_hash.clone(),
                    current_hash: modification.current_hash.clone(),
                    modification_type: match modification.modification_type {
                        ModificationType::HashChanged => "hash_changed".to_string(),
                        ModificationType::MetadataChanged => "metadata_changed".to_string(),
                    },
                    description: modification.describe(),
                });
            }
        }
    }

    Ok(current_metadata)
}

/// Telemetry event emitted when spawn-path binary modification is detected.
#[derive(Debug, Clone)]
pub struct SpawnPathModificationEvent {
    pub path: String,
    pub original_hash: String,
    pub current_hash: String,
    pub modification_type: String,
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binary_metadata_from_current_exe() {
        let metadata = BinaryMetadata::from_current_exe()
            .expect("failed to get current exe metadata");

        assert!(!metadata.path.as_os_str().is_empty());
        assert!(!metadata.hash.is_empty());
        assert!(metadata.inode > 0);
        assert!(metadata.mtime_secs > 0);
        assert!(metadata.size > 0);

        // Hash should be 64 hex characters (SHA-256)
        assert_eq!(metadata.hash.len(), 64);
    }

    #[test]
    fn test_binary_metadata_no_modification() {
        let metadata = BinaryMetadata::from_current_exe()
            .expect("failed to get current exe metadata");

        // Immediately check again — should report no modification
        let modification = metadata.detect_modification()
            .expect("failed to check modification");

        assert!(modification.is_none(), "binary should not be modified immediately after recording");
    }

    #[test]
    fn test_compute_sha256() {
        // Test with a known file
        let exe_path = std::env::current_exe()
            .expect("failed to get current exe");

        let hash = compute_sha256(&exe_path)
            .expect("failed to compute hash");

        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
