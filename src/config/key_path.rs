//! Key path parsing for dot-notation configuration paths.
//!
//! This module provides functionality to parse dot-notation key paths (e.g.,
//! "worker.max_workers") into their component segments for hierarchical
//! configuration access.

use anyhow::{bail, Result};

/// Parses a dot-notation key path into segments.
///
/// Splits the input string on '.' characters to create a vector of path
/// segments. Handles various edge cases appropriately.
///
/// # Arguments
///
/// * `path` - The dot-notation path to parse (e.g., "worker.max_workers")
///
/// # Returns
///
/// * `Ok(Vec<String>)` - Vector of path segments
/// * `Err(anyhow::Error)` - If the path is invalid
///
/// # Examples
///
/// ```
/// use needle::config::key_path::parse_key_path;
///
/// assert_eq!(parse_key_path("worker.max_workers").unwrap(), vec!["worker", "max_workers"]);
/// assert_eq!(parse_key_path("worker").unwrap(), vec!["worker"]);
/// ```
///
/// # Edge Cases
///
/// - Empty string (""): Returns error
/// - Leading dot (".worker"): Returns error
/// - Trailing dot ("worker."): Returns error
/// - Consecutive dots ("worker..max_workers"): Returns error
/// - Single dot ("."): Returns error
pub fn parse_key_path(path: &str) -> Result<Vec<String>> {
    // Empty path is invalid
    if path.is_empty() {
        bail!("empty key path");
    }

    // Check for invalid patterns
    if path.starts_with('.') {
        bail!("key path cannot start with '.'");
    }

    if path.ends_with('.') {
        bail!("key path cannot end with '.'");
    }

    // Check for consecutive dots
    if path.contains("..") {
        bail!("key path cannot contain consecutive dots ('..')");
    }

    // Split on '.' and collect segments
    let segments: Vec<String> = path.split('.').map(|s| s.to_string()).collect();

    // Validate that all segments are non-empty (should be guaranteed by checks above)
    if segments.iter().any(|s| s.is_empty()) {
        bail!("key path contains empty segment");
    }

    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_key() {
        let result = parse_key_path("worker").unwrap();
        assert_eq!(result, vec!["worker"]);
    }

    #[test]
    fn test_parse_nested_key() {
        let result = parse_key_path("worker.max_workers").unwrap();
        assert_eq!(result, vec!["worker", "max_workers"]);
    }

    #[test]
    fn test_parse_deeply_nested_key() {
        let result = parse_key_path("worker.claim.max_parallel").unwrap();
        assert_eq!(result, vec!["worker", "claim", "max_parallel"]);
    }

    #[test]
    fn test_parse_key_with_numbers() {
        let result = parse_key_path("worker.max_workers_2").unwrap();
        assert_eq!(result, vec!["worker", "max_workers_2"]);
    }

    #[test]
    fn test_empty_string_returns_error() {
        let result = parse_key_path("");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "empty key path");
    }

    #[test]
    fn test_leading_dot_returns_error() {
        let result = parse_key_path(".worker");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "key path cannot start with '.'"
        );
    }

    #[test]
    fn test_trailing_dot_returns_error() {
        let result = parse_key_path("worker.");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "key path cannot end with '.'"
        );
    }

    #[test]
    fn test_consecutive_dots_returns_error() {
        let result = parse_key_path("worker..max_workers");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "key path cannot contain consecutive dots ('..')"
        );
    }

    #[test]
    fn test_single_dot_returns_error() {
        let result = parse_key_path(".");
        assert!(result.is_err());
        // A single dot both starts and ends with '.', so we check for startswith first
        assert_eq!(
            result.unwrap_err().to_string(),
            "key path cannot start with '.'"
        );
    }

    #[test]
    fn test_multiple_consecutive_dots_returns_error() {
        let result = parse_key_path("worker...max_workers");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "key path cannot contain consecutive dots ('..')"
        );
    }

    #[test]
    fn test_leading_and_trailing_dots_returns_error() {
        let result = parse_key_path(".worker.");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "key path cannot start with '.'"
        );
    }

    #[test]
    fn test_key_with_underscores() {
        let result = parse_key_path("worker_config.max_workers_per_thread").unwrap();
        assert_eq!(result, vec!["worker_config", "max_workers_per_thread"]);
    }

    #[test]
    fn test_key_with_hyphens() {
        let result = parse_key_path("worker-config.max-workers").unwrap();
        assert_eq!(result, vec!["worker-config", "max-workers"]);
    }
}
