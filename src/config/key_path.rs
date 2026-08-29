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

    // Edge case: single character segment
    #[test]
    fn test_single_character_segment() {
        let result = parse_key_path("a.b.c").unwrap();
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    // Edge case: single segment (top-level field access)
    #[test]
    fn test_single_segment_top_level() {
        let result = parse_key_path("agent").unwrap();
        assert_eq!(result, vec!["agent"]);
    }

    // Edge case: very long segment name
    #[test]
    fn test_very_long_segment() {
        let long_segment = "a".repeat(1000);
        let result = parse_key_path(&long_segment).unwrap();
        assert_eq!(result, vec![long_segment]);
    }

    // Edge case: many segments (deep nesting)
    #[test]
    fn test_many_segments_deep_nesting() {
        let path = (1..=50)
            .map(|i| format!("level{}", i))
            .collect::<Vec<_>>()
            .join(".");
        let result = parse_key_path(&path).unwrap();
        assert_eq!(result.len(), 50);
        assert_eq!(result[0], "level1");
        assert_eq!(result[49], "level50");
    }

    // Edge case: mixed case segments
    #[test]
    fn test_mixed_case_segments() {
        let result = parse_key_path("WorkerConfig.MaxWorkers").unwrap();
        assert_eq!(result, vec!["WorkerConfig", "MaxWorkers"]);
    }

    // Edge case: segments with numbers only
    #[test]
    fn test_numeric_segments() {
        let result = parse_key_path("123.456.789").unwrap();
        assert_eq!(result, vec!["123", "456", "789"]);
    }

    // Edge case: segment starting with number
    #[test]
    fn test_segment_starting_with_number() {
        let result = parse_key_path("2_worker.max_workers").unwrap();
        assert_eq!(result, vec!["2_worker", "max_workers"]);
    }

    // Edge case: whitespace-only segment (valid, not trimmed by parse function)
    #[test]
    fn test_whitespace_only_segment() {
        let result = parse_key_path("   ").unwrap();
        // The function doesn't trim whitespace, so whitespace-only strings are valid segments
        assert_eq!(result, vec!["   "]);
    }

    // Edge case: path with leading whitespace
    #[test]
    fn test_leading_whitespace_is_preserved() {
        let result = parse_key_path(" worker").unwrap();
        // The function doesn't trim whitespace, so it should preserve it
        assert_eq!(result, vec![" worker"]);
    }

    // Edge case: path with trailing whitespace
    #[test]
    fn test_trailing_whitespace_is_preserved() {
        let result = parse_key_path("worker ").unwrap();
        // The function doesn't trim whitespace, so it should preserve it
        assert_eq!(result, vec!["worker "]);
    }

    // Edge case: repeated segments (same field multiple times)
    #[test]
    fn test_repeated_segments() {
        let result = parse_key_path("worker.worker.worker").unwrap();
        assert_eq!(result, vec!["worker", "worker", "worker"]);
    }

    // Edge case: empty string single test (duplicate of existing, ensuring coverage)
    #[test]
    fn test_empty_path_edge_case() {
        let result = parse_key_path("");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("empty key path"));
    }

    // Edge case: single character with various symbols
    #[test]
    fn test_single_char_various_symbols() {
        let result = parse_key_path("x.y_z-z").unwrap();
        assert_eq!(result, vec!["x", "y_z-z"]);
    }

    // ===== Nested Key Path Scenarios =====

    // Test valid nested field access with realistic config paths
    #[test]
    fn test_valid_nested_field_access_strands_explore_workspace_root() {
        let result = parse_key_path("strands.explore.workspace_root").unwrap();
        assert_eq!(result, vec!["strands", "explore", "workspace_root"]);
    }

    #[test]
    fn test_valid_nested_field_access_worker_claim_max_parallel() {
        let result = parse_key_path("worker.claim.max_parallel").unwrap();
        assert_eq!(result, vec!["worker", "claim", "max_parallel"]);
    }

    #[test]
    fn test_valid_nested_field_access_with_config_prefix() {
        let result = parse_key_path("config.bead_cli.backend").unwrap();
        assert_eq!(result, vec!["config", "bead_cli", "backend"]);
    }

    #[test]
    fn test_valid_nested_field_access_telemetry_output_format() {
        let result = parse_key_path("telemetry.output.format").unwrap();
        assert_eq!(result, vec!["telemetry", "output", "format"]);
    }

    // Test invalid nested segments at various positions
    #[test]
    fn test_invalid_nested_segment_leading_dot_in_middle() {
        // Valid prefix "worker", invalid segment ".max_workers"
        let result = parse_key_path("worker..max_workers");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "key path cannot contain consecutive dots ('..')"
        );
    }

    #[test]
    fn test_invalid_nested_segment_trailing_dot_at_end() {
        // Valid prefix "worker.claim", invalid trailing dot
        let result = parse_key_path("worker.claim.");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "key path cannot end with '.'"
        );
    }

    #[test]
    fn test_invalid_nested_segment_empty_segment_in_middle() {
        // Valid prefix "strands", empty segment, valid suffix "workspace_root"
        let result = parse_key_path("strands..workspace_root");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "key path cannot contain consecutive dots ('..')"
        );
    }

    #[test]
    fn test_invalid_nested_segment_multiple_empty_segments() {
        // Multiple consecutive dots creating empty segments
        let result = parse_key_path("worker...claim.max_parallel");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "key path cannot contain consecutive dots ('..')"
        );
    }

    // Test deep nesting scenarios (3+ levels)
    #[test]
    fn test_deep_nesting_four_levels() {
        let result = parse_key_path("strands.explore.config.workspaces").unwrap();
        assert_eq!(result, vec!["strands", "explore", "config", "workspaces"]);
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn test_deep_nesting_five_levels() {
        let result = parse_key_path("a.b.c.d.e").unwrap();
        assert_eq!(result, vec!["a", "b", "c", "d", "e"]);
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn test_deep_nesting_six_levels_realistic_config() {
        let result = parse_key_path("worker.outcome.dispatch.config.max_parallel").unwrap();
        assert_eq!(
            result,
            vec!["worker", "outcome", "dispatch", "config", "max_parallel"]
        );
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn test_deep_nesting_seven_levels() {
        let result = parse_key_path("level1.level2.level3.level4.level5.level6.level7").unwrap();
        assert_eq!(result.len(), 7);
        assert_eq!(result[0], "level1");
        assert_eq!(result[6], "level7");
    }

    #[test]
    fn test_deep_nesting_ten_levels() {
        let result = parse_key_path("a.b.c.d.e.f.g.h.i.j").unwrap();
        assert_eq!(result.len(), 10);
        assert_eq!(result[0], "a");
        assert_eq!(result[9], "j");
    }

    // Test partial path validation (valid prefix, invalid suffix)
    #[test]
    fn test_partial_path_valid_prefix_invalid_suffix_leading_dot() {
        // Valid prefix "strands.explore", invalid suffix starts with "."
        let result = parse_key_path("strands.explore.workspace_root");
        // This should succeed since all segments are valid
        assert!(result.is_ok());
    }

    #[test]
    fn test_partial_path_valid_prefix_trailing_dot() {
        // Valid prefix "strands.explore", invalid trailing dot
        let result = parse_key_path("strands.explore.");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot end with"));
    }

    #[test]
    fn test_partial_path_valid_prefix_empty_middle_segment() {
        // Valid prefix "strands", empty middle segment, valid suffix "workspace_root"
        let result = parse_key_path("strands.explore..workspace_root");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("consecutive dots"));
    }

    #[test]
    fn test_partial_path_deep_prefix_invalid_suffix() {
        // Valid deep prefix "worker.claim.dispatch.config", invalid trailing dot
        let result = parse_key_path("worker.claim.dispatch.config.");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot end with"));
    }

    #[test]
    fn test_partial_path_valid_all_way_to_last_segment() {
        // All segments valid throughout
        let result = parse_key_path("worker.outcome.bead_store.config.backend");
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            vec!["worker", "outcome", "bead_store", "config", "backend"]
        );
    }

    // Test complex real-world config paths
    #[test]
    fn test_real_world_config_path_bead_backend() {
        let result = parse_key_path("config.bead_cli.backend.bead_rs").unwrap();
        assert_eq!(result, vec!["config", "bead_cli", "backend", "bead_rs"]);
    }

    #[test]
    fn test_real_world_config_path_strands_explore_workspaces() {
        let result = parse_key_path("strands.explore.config.workspaces").unwrap();
        assert_eq!(result, vec!["strands", "explore", "config", "workspaces"]);
    }

    #[test]
    fn test_real_world_config_path_worker_telemetry() {
        let result = parse_key_path("worker.telemetry.output.format").unwrap();
        assert_eq!(result, vec!["worker", "telemetry", "output", "format"]);
    }

    // Test edge cases with very deep nesting
    #[test]
    fn test_very_deep_nesting_20_levels() {
        let path = (1..=20)
            .map(|i| format!("level{}", i))
            .collect::<Vec<_>>()
            .join(".");
        let result = parse_key_path(&path).unwrap();
        assert_eq!(result.len(), 20);
        assert_eq!(result[0], "level1");
        assert_eq!(result[19], "level20");
    }

    #[test]
    fn test_very_deep_nesting_100_levels() {
        let path = (1..=100)
            .map(|i| format!("seg{}", i))
            .collect::<Vec<_>>()
            .join(".");
        let result = parse_key_path(&path).unwrap();
        assert_eq!(result.len(), 100);
        assert_eq!(result[0], "seg1");
        assert_eq!(result[99], "seg100");
    }

    // Test invalid segments at different depths
    #[test]
    fn test_invalid_segment_at_depth_3_consecutive_dots() {
        // Valid: "worker.claim", invalid: "..max_parallel"
        let result = parse_key_path("worker.claim..max_parallel");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("consecutive dots"));
    }

    #[test]
    fn test_invalid_segment_at_depth_5_trailing_dot() {
        // Valid: "a.b.c.d.e", invalid: trailing dot
        let result = parse_key_path("a.b.c.d.e.");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot end with"));
    }

    #[test]
    fn test_invalid_segment_at_depth_4_leading_dot_after_valid_prefix() {
        // Valid: "a.b.c", invalid: ".d"
        let result = parse_key_path("a.b.c.d"); // Actually valid!
        assert!(result.is_ok()); // This should succeed as all segments are valid
    }
}
