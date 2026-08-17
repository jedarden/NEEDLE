//! Build metadata tracking for needle binaries.
//!
//! Provides functionality to read build-time metadata from the current
//! running binary and from on-disk binaries. Metadata includes:
//! - Version (from Cargo.toml)
//! - Git commit SHA (embedded at build time)
//! - Build timestamp (embedded at build time)

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Build metadata embedded in the needle binary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildMetadata {
    /// Version from Cargo.toml
    pub version: String,
    /// Git commit SHA (short form) at build time
    pub commit_sha: String,
    /// Build timestamp in UTC (ISO 8601 format)
    pub build_timestamp: String,
}

impl BuildMetadata {
    /// Get the build metadata of the current running binary.
    ///
    /// This reads the compile-time environment variables set by build.rs
    /// and combines them with the Cargo version.
    pub fn current() -> Self {
        BuildMetadata {
            version: env!("CARGO_PKG_VERSION").to_string(),
            commit_sha: env!("NEEDLE_COMMIT_SHA").to_string(),
            build_timestamp: env!("NEEDLE_BUILD_TIMESTAMP").to_string(),
        }
    }

    /// Read build metadata from a binary file on disk.
    ///
    /// This function searches the binary file for the embedded metadata
    /// section. The metadata is embedded as a JSON string at compile time.
    ///
    /// # Arguments
    /// * `path` - Path to the binary file
    ///
    /// # Returns
    /// * `Ok(BuildMetadata)` if metadata is found and valid
    /// * `Err` if the file cannot be read or metadata is invalid
    pub fn from_binary(path: &Path) -> Result<Self> {
        // Read the binary file
        let data = fs::read(path)
            .with_context(|| format!("failed to read binary file: {}", path.display()))?;

        // Try to extract metadata from the binary
        Self::extract_from_binary_data(&data)
            .with_context(|| format!("failed to extract metadata from binary: {}", path.display()))
    }

    /// Extract build metadata from raw binary data.
    ///
    /// Searches for the metadata marker in the binary and parses the JSON.
    fn extract_from_binary_data(data: &[u8]) -> Result<Self> {
        // Convert bytes to string for searching
        // We'll look for a pattern that indicates the start of metadata
        let binary_str = String::from_utf8_lossy(data);

        // The metadata is embedded as a JSON string with a specific marker
        // We'll search for patterns that look like our metadata structure
        // Since the exact embedding varies, we'll search for JSON-like patterns

        // Look for JSON patterns that match our structure
        // We'll search for patterns containing our field names
        let search_patterns = [r#""commit_sha":"#, r#""build_timestamp":"#, r#""version":"#];

        for pattern in search_patterns.iter() {
            if let Some(pos) = binary_str.find(pattern) {
                // Try to extract JSON from this position
                // Look backwards to find the start of the JSON object
                let start_pos = binary_str[..pos].rfind('{').unwrap_or(0);

                // Look forwards to find the end of the JSON object
                let remaining = &binary_str[start_pos..];
                let mut brace_count = 0;
                let mut end_pos = 0;

                for (i, ch) in remaining.chars().enumerate() {
                    match ch {
                        '{' => brace_count += 1,
                        '}' => {
                            brace_count -= 1;
                            if brace_count == 0 {
                                end_pos = i + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }

                if end_pos > 0 {
                    let json_str = &remaining[..end_pos];

                    // Try to parse as JSON
                    if let Ok(metadata) = serde_json::from_str::<BuildMetadata>(json_str) {
                        return Ok(metadata);
                    }
                }
            }
        }

        // If we couldn't find embedded metadata, try to read version strings
        // as a fallback for basic identification
        Self::extract_from_string_fallback(&binary_str)
    }

    /// Fallback method to extract basic metadata from string patterns.
    ///
    /// This is used when the embedded JSON metadata is not found.
    fn extract_from_string_fallback(data: &str) -> Result<Self> {
        // Look for version-like patterns
        let version = if let Some(pos) = data.find("needle ") {
            let remaining = &data[pos + 7..];
            let version_end = remaining
                .find(|c: char| !c.is_ascii_digit() && c != '.')
                .unwrap_or(remaining.len());
            remaining[..version_end].to_string()
        } else {
            "unknown".to_string()
        };

        Ok(BuildMetadata {
            version,
            commit_sha: "unknown".to_string(),
            build_timestamp: "unknown".to_string(),
        })
    }

    /// Format the metadata as a human-readable string.
    pub fn format_display(&self) -> String {
        if self.commit_sha == "unknown" || self.build_timestamp == "unknown" {
            format!("needle {}", self.version)
        } else {
            format!(
                "needle {} (commit {}, built {})",
                self.version, self.commit_sha, self.build_timestamp
            )
        }
    }

    /// Get the version string only.
    pub fn version_only(&self) -> String {
        format!("needle {}", self.version)
    }

    /// Read build metadata from the latest needle-stable binary on disk.
    ///
    /// This function attempts to read metadata from the needle-stable binary
    /// located in the needle home directory.
    ///
    /// # Returns
    /// * `Ok(Some(BuildMetadata))` if the stable binary exists and metadata is readable
    /// * `Ok(None)` if the stable binary does not exist
    /// * `Err` if the binary exists but cannot be read or parsed
    pub fn from_stable_binary() -> Result<Option<Self>> {
        // Get the needle home directory
        let home = needle_home()?;
        let stable_binary = home.join("bin").join("needle-stable");

        // Check if the stable binary exists
        if !stable_binary.exists() {
            return Ok(None);
        }

        // Read metadata from the stable binary
        let metadata = Self::from_binary(&stable_binary)?;
        Ok(Some(metadata))
    }
}

/// Get the needle home directory.
///
/// Returns the path to the needle home directory, defaulting to
/// ~/.needle if the NEEDLE_HOME environment variable is not set.
fn needle_home() -> Result<PathBuf> {
    if let Ok(home) = std::env::var("NEEDLE_HOME") {
        Ok(PathBuf::from(home))
    } else {
        let default_home = dirs_or_home(".needle");
        Ok(default_home)
    }
}

/// Resolve a path relative to the user's home directory.
///
/// If HOME is set, uses that. Otherwise, uses /tmp as a fallback.
fn dirs_or_home(relative: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(relative)
    } else {
        PathBuf::from("/tmp").join(relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_current_metadata() {
        let metadata = BuildMetadata::current();

        // Should have non-empty version
        assert!(!metadata.version.is_empty());

        // Commit SHA should be set (even if "unknown")
        assert!(!metadata.commit_sha.is_empty());

        // Build timestamp should be set (even if "unknown")
        assert!(!metadata.build_timestamp.is_empty());
    }

    #[test]
    fn test_metadata_display() {
        let metadata = BuildMetadata {
            version: "1.0.0".to_string(),
            commit_sha: "abc123".to_string(),
            build_timestamp: "2024-01-01T00:00:00Z".to_string(),
        };

        let display = metadata.format_display();
        assert!(display.contains("1.0.0"));
        assert!(display.contains("abc123"));
        assert!(display.contains("2024-01-01T00:00:00Z"));
    }

    #[test]
    fn test_metadata_display_unknown() {
        let metadata = BuildMetadata {
            version: "1.0.0".to_string(),
            commit_sha: "unknown".to_string(),
            build_timestamp: "unknown".to_string(),
        };

        let display = metadata.format_display();
        assert_eq!(display, "needle 1.0.0");
    }

    #[test]
    fn test_version_only() {
        let metadata = BuildMetadata {
            version: "2.0.0".to_string(),
            commit_sha: "def456".to_string(),
            build_timestamp: "2024-02-01T12:00:00Z".to_string(),
        };

        assert_eq!(metadata.version_only(), "needle 2.0.0");
    }

    #[test]
    fn test_from_binary_with_embedded_metadata() {
        // Create a fake binary with embedded metadata
        let mut temp_file = NamedTempFile::new().unwrap();

        let metadata = BuildMetadata {
            version: "1.2.3".to_string(),
            commit_sha: "test123".to_string(),
            build_timestamp: "2024-06-15T10:30:00Z".to_string(),
        };

        let metadata_json = serde_json::to_string(&metadata).unwrap();

        // Write some fake binary data
        writeln!(temp_file, "BINARY_DATA_HERE").unwrap();
        // Embed the metadata
        writeln!(temp_file, "{}", metadata_json).unwrap();
        writeln!(temp_file, "MORE_BINARY_DATA").unwrap();

        // Try to read it back
        let read_metadata = BuildMetadata::from_binary(temp_file.path()).unwrap();

        assert_eq!(read_metadata.version, "1.2.3");
        assert_eq!(read_metadata.commit_sha, "test123");
        assert_eq!(read_metadata.build_timestamp, "2024-06-15T10:30:00Z");
    }

    #[test]
    fn test_from_binary_fallback() {
        // Create a fake binary without proper embedded metadata
        let mut temp_file = NamedTempFile::new().unwrap();

        // Write just version information
        writeln!(temp_file, "needle 3.0.0").unwrap();

        // Try to read it back - should use fallback
        let read_metadata = BuildMetadata::from_binary(temp_file.path()).unwrap();

        assert_eq!(read_metadata.version, "3.0.0");
        assert_eq!(read_metadata.commit_sha, "unknown");
        assert_eq!(read_metadata.build_timestamp, "unknown");
    }

    #[test]
    fn test_from_binary_nonexistent_file() {
        let result = BuildMetadata::from_binary(Path::new("/nonexistent/file"));
        assert!(result.is_err());
    }

    #[test]
    fn test_metadata_serialization() {
        let metadata = BuildMetadata {
            version: "1.0.0".to_string(),
            commit_sha: "abc123".to_string(),
            build_timestamp: "2024-01-01T00:00:00Z".to_string(),
        };

        // Test JSON serialization
        let json = serde_json::to_string(&metadata).unwrap();
        assert!(json.contains(r#""version":"1.0.0""#));
        assert!(json.contains(r#""commit_sha":"abc123""#));
        assert!(json.contains(r#""build_timestamp":"2024-01-01T00:00:00Z""#));

        // Test JSON deserialization
        let deserialized: BuildMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, metadata);
    }

    #[test]
    fn test_from_stable_binary_no_file() {
        // Env mutation must hold the crate-wide env lock (see util::test_env):
        // an unsynchronized set_var races every concurrent test and spawn.
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        // Create a temporary directory to use as needle home
        let temp_dir = tempfile::tempdir().unwrap();
        let needle_home = temp_dir.path();

        // Set NEEDLE_HOME to the temp directory
        std::env::set_var("NEEDLE_HOME", needle_home);

        // Create bin directory but no stable binary
        let bin_dir = needle_home.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        // Should return None when no stable binary exists
        let result = BuildMetadata::from_stable_binary().unwrap();
        assert!(result.is_none());

        // Clean up
        std::env::remove_var("NEEDLE_HOME");
    }

    #[test]
    fn test_from_stable_binary_with_file() {
        // Env mutation must hold the crate-wide env lock (see util::test_env).
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        // Create a temporary directory to use as needle home
        let temp_dir = tempfile::tempdir().unwrap();
        let needle_home = temp_dir.path();

        // Set NEEDLE_HOME to the temp directory
        std::env::set_var("NEEDLE_HOME", needle_home);

        // Create bin directory
        let bin_dir = needle_home.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        // Create a fake stable binary with metadata
        let stable_binary = bin_dir.join("needle-stable");
        let metadata = BuildMetadata {
            version: "5.0.0".to_string(),
            commit_sha: "stable123".to_string(),
            build_timestamp: "2024-12-01T08:00:00Z".to_string(),
        };

        let metadata_json = serde_json::to_string(&metadata).unwrap();
        std::fs::write(
            &stable_binary,
            format!("BINARY_DATA\n{}\nMORE_DATA", metadata_json),
        )
        .unwrap();

        // Should return Some(metadata) when stable binary exists
        let result = BuildMetadata::from_stable_binary().unwrap();
        assert!(result.is_some());

        let read_metadata = result.unwrap();
        assert_eq!(read_metadata.version, "5.0.0");
        assert_eq!(read_metadata.commit_sha, "stable123");
        assert_eq!(read_metadata.build_timestamp, "2024-12-01T08:00:00Z");

        // Clean up
        std::env::remove_var("NEEDLE_HOME");
    }
}
