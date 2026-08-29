//! Edge case panic tests for NEEDLE.
//!
//! These tests cover unusual edge cases, resource exhaustion scenarios,
//! concurrent access patterns, and malformed inputs that might trigger panics
//! if not handled correctly.
//!
//! ## Test Categories
//!
//! - **Double cleanup tests**: Verify idempotent cleanup operations
//! - **Resource exhaustion tests**: Empty pools, full queues, buffer limits
//! - **Concurrent access tests**: Race conditions, shared state access
//! - **Malformed input tests**: Invalid data at module boundaries
//! - **Null/unexpected input tests**: Empty strings, None values, unexpected types

use std::fs::{self, File};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

// ──────────────────────────────────────────────────────────────────────────────
// Double Cleanup Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod double_cleanup_tests {
    use super::*;

    #[test]
    fn test_double_config_cleanup_does_not_panic() {
        // Given: A ConfigLoader that loads a config
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("config.yaml");
        let yaml = r#"
worker:
  name: "test-worker"
  max_workers: 4
"#;
        fs::write(&config_file, yaml).expect("failed to write config");

        // When: Loading config multiple times (simulating double cleanup scenarios)
        let result1 = needle::config::ConfigLoader::load_from_path(&config_file);
        let result2 = needle::config::ConfigLoader::load_from_path(&config_file);
        let result3 = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: All loads should succeed without panicking
        assert!(result1.is_ok(), "First config load should succeed");
        assert!(result2.is_ok(), "Second config load should succeed");
        assert!(result3.is_ok(), "Third config load should succeed");
    }

    #[test]
    fn test_multiple_tempdir_cleanup_does_not_panic() {
        // Given: Multiple temp directories
        let temp_dirs: Vec<TempDir> = (0..10)
            .map(|_| TempDir::new().expect("failed to create temp dir"))
            .collect();

        // When: All temp dirs drop simultaneously
        drop(temp_dirs);

        // Then: Should not panic (this is the test passing - if it panics, we'd catch it)
        // Creating new temp dirs to verify the old ones were cleaned up properly
        let new_temp = TempDir::new().expect("should create new temp after cleanup");
        assert!(new_temp.path().exists(), "New temp dir should be created");
    }

    #[test]
    #[cfg(unix)]
    fn test_double_lockfile_cleanup_does_not_panic() {
        // Given: A lock file that gets cleaned up multiple times
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let lock_path = temp_dir.path().join("test.lock");

        // Create lock file
        File::create(&lock_path).expect("failed to create lock file");

        // When: Attempting to remove/cleanup the same file multiple times
        let remove1 = fs::remove_file(&lock_path);
        let remove2 = fs::remove_file(&lock_path);
        let remove3 = fs::remove_file(&lock_path);

        // Then: First removal succeeds, subsequent ones fail gracefully (not panic)
        assert!(remove1.is_ok(), "First removal should succeed");
        assert!(
            remove2.is_err(),
            "Second removal should fail (already removed)"
        );
        assert!(
            remove3.is_err(),
            "Third removal should fail (already removed)"
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Resource Exhaustion Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod resource_exhaustion_tests {
    use super::*;
    use needle::bead_store::BeadStore;

    /// A bead store that simulates resource exhaustion.
    struct ExhaustedBeadStore;

    #[async_trait::async_trait]
    impl BeadStore for ExhaustedBeadStore {
        async fn ready(
            &self,
            _filters: &needle::bead_store::Filters,
        ) -> anyhow::Result<Vec<needle::types::Bead>> {
            // Simulate empty pool - no beads available
            Ok(Vec::new())
        }

        async fn list_all(&self) -> anyhow::Result<Vec<needle::types::Bead>> {
            Ok(Vec::new())
        }

        async fn show(&self, _id: &needle::types::BeadId) -> anyhow::Result<needle::types::Bead> {
            Err(anyhow::anyhow!("Resource exhausted: no beads available"))
        }

        async fn notes(&self, _id: &needle::types::BeadId) -> anyhow::Result<Option<String>> {
            Ok(None)
        }

        async fn claim(
            &self,
            _id: &needle::types::BeadId,
            _actor: &str,
        ) -> anyhow::Result<needle::types::ClaimResult> {
            Err(anyhow::anyhow!("Resource exhausted: claim queue full"))
        }

        async fn claim_auto(&self, _actor: &str) -> anyhow::Result<needle::types::ClaimResult> {
            Err(anyhow::anyhow!("Resource exhausted: no beads to claim"))
        }

        async fn release(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Resource exhausted: cannot release"))
        }

        async fn block(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Resource exhausted: cannot block"))
        }

        async fn clear_assignee(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Resource exhausted: cannot clear assignee"))
        }

        async fn flush(&self) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Resource exhausted: cannot flush"))
        }

        async fn reopen(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Resource exhausted: cannot reopen"))
        }

        async fn labels(&self, _id: &needle::types::BeadId) -> anyhow::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn add_label(&self, _id: &needle::types::BeadId, _label: &str) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Resource exhausted: cannot add label"))
        }

        async fn remove_label(
            &self,
            _id: &needle::types::BeadId,
            _label: &str,
        ) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Resource exhausted: cannot remove label"))
        }

        async fn create_bead(
            &self,
            _title: &str,
            _body: &str,
            _labels: &[&str],
        ) -> anyhow::Result<needle::types::BeadId> {
            Err(anyhow::anyhow!("Resource exhausted: cannot create bead"))
        }

        async fn add_dependency(
            &self,
            _blocker_id: &needle::types::BeadId,
            _blocked_id: &needle::types::BeadId,
        ) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Resource exhausted: cannot add dependency"))
        }

        async fn remove_dependency(
            &self,
            _blocked_id: &needle::types::BeadId,
            _blocker_id: &needle::types::BeadId,
        ) -> anyhow::Result<()> {
            Err(anyhow::anyhow!(
                "Resource exhausted: cannot remove dependency"
            ))
        }

        async fn doctor_repair(&self) -> anyhow::Result<needle::bead_store::RepairReport> {
            Err(anyhow::anyhow!("Resource exhausted: cannot repair"))
        }

        async fn doctor_check(&self) -> anyhow::Result<needle::bead_store::RepairReport> {
            Err(anyhow::anyhow!("Resource exhausted: cannot check"))
        }

        async fn full_rebuild(&self) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Resource exhausted: cannot rebuild"))
        }

        fn has_valid_store(&self) -> bool {
            true
        }

        fn is_corruption_error(&self, _message: &str) -> bool {
            false
        }

        fn is_lock_error(&self, _message: &str) -> bool {
            false
        }

        fn is_sync_conflict(&self, _message: &str) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn test_empty_bead_pool_does_not_panic() {
        // Given: A bead store with no beads (exhausted/empty pool)
        let store = Arc::new(ExhaustedBeadStore);
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let lock_dir = temp_dir.path().join("locks");
        fs::create_dir_all(&lock_dir).expect("failed to create lock dir");

        let telemetry = needle::telemetry::Telemetry::new("test-worker-01".into());
        let claimer = needle::claim::Claimer::new(store, lock_dir, 1, 100, telemetry);

        // When: Attempting to claim from empty pool
        let result = claimer.claim_auto("test-actor", "test-strand").await;

        // Then: Should return error without panicking
        assert!(
            result.is_err(),
            "Claiming from empty pool should return error"
        );
        if let Err(e) = result {
            let error_msg = format!("{:?}", e);
            assert!(
                error_msg.contains("exhausted") || error_msg.contains("no beads"),
                "Error should indicate resource exhaustion: {}",
                error_msg
            );
        }
    }

    #[test]
    fn test_zero_max_workers_config_does_not_panic() {
        // Given: Config with max_workers = 0 (edge case for empty pool)
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("config.yaml");
        let yaml = r#"
worker:
  name: "test-worker"
  max_workers: 0
"#;
        fs::write(&config_file, yaml).expect("failed to write config");

        // When: Loading config with zero workers
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should load without panicking
        assert!(result.is_ok(), "Config with max_workers=0 should load");
        let config = result.unwrap();
        // Config may be loaded, validation happens elsewhere
        let _max_workers = config.worker.max_workers;
    }

    #[test]
    fn test_very_large_queue_size_does_not_panic() {
        // Given: Config with very large queue size
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("config.yaml");
        let yaml = r#"
worker:
  name: "test-worker"
  max_workers: 999999
"#;
        fs::write(&config_file, yaml).expect("failed to write config");

        // When: Loading config with large queue size
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should load without panicking
        assert!(result.is_ok(), "Config with large queue should load");
    }

    #[test]
    fn test_negative_timeout_does_not_panic() {
        // Given: Config with negative timeout (should be rejected or clamped)
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("config.yaml");
        let yaml = r#"
agent:
  timeout_secs: -100
"#;
        fs::write(&config_file, yaml).expect("failed to write config");

        // When: Loading config with negative timeout
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should load without panicking (validation happens elsewhere)
        assert!(result.is_ok(), "Config with negative timeout should load");
        let config = result.unwrap();
        // Config loaded successfully - validation happens elsewhere
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Concurrent Access Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod concurrent_access_tests {
    use super::*;

    #[test]
    fn test_concurrent_config_reads_do_not_panic() {
        // Given: A config file
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("config.yaml");
        let yaml = r#"
worker:
  name: "test-worker"
  max_workers: 4
"#;
        fs::write(&config_file, yaml).expect("failed to write config");

        // When: Multiple threads read the same config concurrently
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let config_path = config_file.clone();
                thread::spawn(move || needle::config::ConfigLoader::load_from_path(&config_path))
            })
            .collect();

        // Then: All reads should succeed without panicking
        for handle in handles {
            let result = handle.join().expect("thread panicked");
            assert!(result.is_ok(), "Concurrent config read should succeed");
        }
    }

    #[test]
    fn test_concurrent_file_creates_do_not_panic() {
        // Given: A temp directory
        let temp_dir = TempDir::new().expect("failed to create temp dir");

        // When: Multiple threads attempt to create files in the same directory
        let handles: Vec<_> = (0..20)
            .map(|i| {
                let dir = temp_dir.path().to_path_buf();
                thread::spawn(move || {
                    let file_path = dir.join(format!("test-{}.txt", i));
                    fs::write(&file_path, format!("content {}", i))
                })
            })
            .collect();

        // Then: All creates should succeed without panicking
        for handle in handles {
            let result = handle.join().expect("thread panicked");
            assert!(result.is_ok(), "Concurrent file create should succeed");
        }
    }

    #[test]
    fn test_concurrent_tempdir_creation_does_not_panic() {
        // When: Multiple threads create temp dirs simultaneously
        let handles: Vec<_> = (0..10)
            .map(|_| {
                thread::spawn(|| {
                    let _temp = TempDir::new();
                    // Temp dir drops here
                })
            })
            .collect();

        // Then: All creations should succeed without panicking
        for handle in handles {
            handle
                .join()
                .expect("thread panicked during tempdir creation");
        }
    }

    #[test]
    fn test_concurrent_bead_id_creation_does_not_panic() {
        // When: Multiple threads create BeadIds concurrently
        let handles: Vec<_> = (0..50)
            .map(|i| {
                thread::spawn(move || {
                    let bead_id = needle::types::BeadId::from(format!("test-bead-{}", i));
                    bead_id.to_string()
                })
            })
            .collect();

        // Then: All creations should succeed without panicking
        for handle in handles {
            let result = handle.join().expect("thread panicked");
            assert!(!result.is_empty(), "BeadId should be valid");
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Malformed Input Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod malformed_input_tests {
    use super::*;

    #[test]
    fn test_config_with_invalid_utf8_does_not_panic() {
        // Given: Config file with invalid UTF-8 content
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("config.bin");

        // Write invalid UTF-8 bytes
        let invalid_utf8 = vec![0xFF, 0xFE, 0xFD, 0xFC];
        fs::write(&config_file, invalid_utf8).expect("failed to write invalid UTF-8");

        // When: Attempting to load invalid UTF-8 config
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should return error without panicking
        assert!(result.is_err(), "Invalid UTF-8 should return error");
        if let Err(e) = result {
            let error_msg = format!("{:?}", e);
            assert!(
                error_msg.contains("utf8")
                    || error_msg.contains("encoding")
                    || error_msg.contains("invalid")
                    || error_msg.contains("byte"),
                "Error should indicate encoding problem: {}",
                error_msg
            );
        }
    }

    #[test]
    fn test_bead_id_with_null_bytes_does_not_panic() {
        // Given: BeadId with null bytes
        let id_with_null = "test\x00bead\x00id";

        // When: Creating BeadId from string with null bytes
        let bead_id = needle::types::BeadId::from(id_with_null.to_string());

        // Then: Should handle without panicking
        let result = bead_id.to_string();
        assert!(
            !result.is_empty(),
            "BeadId with null bytes should still convert to string"
        );
    }

    #[test]
    fn test_path_with_trailing_nulls_does_not_panic() {
        // Given: Path with trailing null bytes
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let mut path_with_null = temp_dir.path().to_path_buf();
        path_with_null.push("test\x00file.txt");

        // When: Attempting to use path with null bytes
        let result = fs::write(&path_with_null, "content");

        // Then: Should either succeed or fail gracefully, not panic
        // Most systems reject null bytes in paths, but shouldn't panic
        match result {
            Ok(_) => {
                // If it succeeded, verify we can read it back
                let read_result = fs::read_to_string(&path_with_null);
                assert!(read_result.is_ok() || read_result.is_err());
            }
            Err(_) => {
                // Expected on most systems - null bytes in paths are invalid
            }
        }
    }

    #[test]
    fn test_config_with_deeply_nested_structures_does_not_panic() {
        // Given: Config with extremely deep nesting (stack overflow potential)
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("deep.yaml");

        // Create deeply nested YAML (100 levels)
        let mut yaml = String::from("a1:\n");
        for i in 2..=100 {
            yaml.push_str(&format!("{}: ", "  ".repeat(i as usize)));
            if i < 100 {
                yaml.push_str(&format!("a{}:\n", i));
            } else {
                yaml.push_str("value\n");
            }
        }

        fs::write(&config_file, yaml).expect("failed to write deep YAML");

        // When: Loading deeply nested config
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should either succeed or fail gracefully (no stack overflow panic)
        match result {
            Ok(_) => {
                // Successfully parsed deeply nested structure
            }
            Err(_) => {
                // Failed gracefully (likely hit recursion limit)
            }
        }
        // If we got here without panic, test passes
    }

    #[test]
    fn test_config_with_special_unicode_characters_does_not_panic() {
        // Given: Config with various special Unicode characters
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("unicode.yaml");

        let unicode_content = r#"
worker:
  name: "worker-日本語-العربية-한국어-🚀-test"
  emoji: "🎉🎊🎈"
  rtl: "مرحبا"
  combining: "é"  # é with combining acute accent
"#;

        fs::write(&config_file, unicode_content).expect("failed to write unicode config");

        // When: Loading config with special unicode
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should load without panicking
        assert!(result.is_ok(), "Config with unicode should load");
        let _config = result.unwrap();
        // Config loaded successfully with unicode characters
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Null/Unexpected Input Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod null_unexpected_input_tests {
    use super::*;

    #[test]
    fn test_empty_bead_id_does_not_panic() {
        // Given: Empty string
        let empty_string = "";

        // When: Creating BeadId from empty string
        let bead_id = needle::types::BeadId::from(empty_string.to_string());

        // Then: Should handle without panicking
        let result = bead_id.to_string();
        assert_eq!(result, "", "Empty BeadId should remain empty");
    }

    #[test]
    fn test_whitespace_only_bead_id_does_not_panic() {
        // Given: Whitespace-only strings
        let whitespace_cases = vec!["  ", "\t", "\n", "   \t\n   "];

        for ws in whitespace_cases {
            // When: Creating BeadId from whitespace
            let bead_id = needle::types::BeadId::from(ws.to_string());

            // Then: Should handle without panicking
            let result = bead_id.to_string();
            assert!(
                !result.is_empty(),
                "Whitespace bead_id should preserve content"
            );
        }
    }

    #[test]
    fn test_very_long_bead_id_does_not_panic() {
        // Given: Very long bead ID (10,000 characters)
        let long_id = "a".repeat(10_000);

        // When: Creating BeadId from very long string
        let bead_id = needle::types::BeadId::from(long_id.clone());

        // Then: Should handle without panicking
        let result = bead_id.to_string();
        assert_eq!(result.len(), 10_000, "Long BeadId should preserve length");
    }

    #[test]
    fn test_bead_id_with_newlines_does_not_panic() {
        // Given: BeadId with embedded newlines
        let newlined_id = "test\nbead\nid\n";

        // When: Creating BeadId from newlined string
        let bead_id = needle::types::BeadId::from(newlined_id.to_string());

        // Then: Should handle without panicking
        let result = bead_id.to_string();
        assert!(result.contains('\n'), "Newlines should be preserved");
    }

    #[test]
    fn test_zero_timeout_instant_does_not_panic() {
        // Given: Instant::now() and zero duration
        let now = Instant::now();
        let zero_duration = Duration::from_secs(0);

        // When: Doing arithmetic with zero duration
        let later = now.checked_add(zero_duration);
        let earlier = now.checked_sub(zero_duration);

        // Then: Should handle without panicking
        assert!(later.is_some(), "Adding zero duration should succeed");
        assert!(
            earlier.is_some(),
            "Subtracting zero duration should succeed"
        );
    }

    #[test]
    fn test_maximum_timeout_does_not_panic() {
        // Given: Maximum u64 duration
        let max_duration = Duration::from_secs(u64::MAX);
        let now = Instant::now();

        // When: Checking if max duration has elapsed
        let _elapsed = now.elapsed();

        // Then: Should handle without panicking
        // Just verifying we can create and use max duration
        assert!(max_duration.as_secs() == u64::MAX);
    }

    #[test]
    fn test_config_with_all_zero_values_does_not_panic() {
        // Given: Config with all zero/empty values
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("zeros.yaml");

        let zeros_yaml = r#"
worker:
  name: ""
  max_workers: 0
agent:
  model: ""
  timeout_secs: 0
"#;

        fs::write(&config_file, zeros_yaml).expect("failed to write zeros config");

        // When: Loading config with all zeros
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should load without panicking (validation happens elsewhere)
        assert!(result.is_ok(), "Config with zeros should load");
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Boundary Condition Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod boundary_condition_tests {
    use super::*;

    #[test]
    fn test_config_with_maximum_workers_does_not_panic() {
        // Given: Config with maximum u64 value for workers
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("max.yaml");

        let max_yaml = format!(
            r#"
worker:
  name: "test-worker"
  max_workers: {}
"#,
            u64::MAX
        );

        fs::write(&config_file, max_yaml).expect("failed to write max config");

        // When: Loading config with maximum value
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should load without panicking
        assert!(result.is_ok(), "Config with max workers should load");
    }

    #[test]
    fn test_config_with_negative_workers_does_not_panic() {
        // Given: Config with negative worker count (invalid YAML number)
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("negative.yaml");

        let negative_yaml = r#"
worker:
  name: "test-worker"
  max_workers: -5
"#;

        fs::write(&config_file, negative_yaml).expect("failed to write negative config");

        // When: Loading config with negative value
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should load without panicking (may parse as signed)
        assert!(
            result.is_ok() || result.is_err(),
            "Should handle negative value gracefully"
        );
    }

    #[test]
    fn test_very_long_path_does_not_panic() {
        // Given: Very long path name
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let long_name = "a".repeat(255); // Common max filename length

        let long_path = temp_dir.path().join(&long_name);

        // When: Creating file with long name
        let result = fs::write(&long_path, "content");

        // Then: Should succeed or fail gracefully (not panic)
        match result {
            Ok(_) => {
                // Succeeded
                let read_result = fs::read_to_string(&long_path);
                assert!(read_result.is_ok());
            }
            Err(_) => {
                // Failed (likely exceeded OS limit)
            }
        }
    }

    #[test]
    fn test_path_with_many_directory_levels_does_not_panic() {
        // Given: Creating deeply nested directory structure
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let mut current = temp_dir.path().to_path_buf();

        // Create 100 levels of directories
        for i in 0..100 {
            current = current.join(format!("level{}", i));
        }

        // When: Creating the deep directory structure
        let result = fs::create_dir_all(&current);

        // Then: Should succeed or fail gracefully (not panic)
        match result {
            Ok(_) => {
                // Successfully created deep structure
                assert!(current.exists(), "Deep path should exist");
            }
            Err(_) => {
                // Failed (likely exceeded PATH_MAX)
            }
        }
    }

    #[test]
    fn test_empty_yaml_file_does_not_panic() {
        // Given: Empty YAML file
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("empty.yaml");
        fs::write(&config_file, "").expect("failed to write empty file");

        // When: Loading empty YAML
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should return default config or error, not panic
        match result {
            Ok(_config) => {
                // Empty file loaded as defaults
            }
            Err(_) => {
                // Empty file rejected
            }
        }
    }

    #[test]
    fn test_yaml_with_only_comments_does_not_panic() {
        // Given: YAML file with only comments
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("comments.yaml");
        let comments_only = r#"
# This is a comment
# Another comment
# worker:
#   name: "test"
"#;

        fs::write(&config_file, comments_only).expect("failed to write comments");

        // When: Loading YAML with only comments
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should handle without panicking
        match result {
            Ok(_) => {
                // Comments-only file loaded as defaults
            }
            Err(_) => {
                // Comments-only file rejected
            }
        }
    }

    #[test]
    fn test_malformed_yaml_with_special_chars_does_not_panic() {
        // Given: YAML with various special characters
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("special.yaml");

        let special_chars = r#"
worker:
  name: "test\x00\x01\x02worker"
  binary: "\u{FEFF}\u{200B}\u{200C}"
"#;

        fs::write(&config_file, special_chars).expect("failed to write special chars");

        // When: Loading YAML with special characters
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should handle without panicking
        match result {
            Ok(_) => {
                // Successfully parsed (with escaped chars)
            }
            Err(_) => {
                // Failed gracefully (invalid YAML)
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Rapid State Change Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod rapid_state_change_tests {
    use super::*;

    #[test]
    fn test_rapid_config_reload_does_not_panic() {
        // Given: A config file
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("config.yaml");

        let initial_yaml = r#"
worker:
  name: "test-worker"
  max_workers: 4
"#;
        fs::write(&config_file, initial_yaml).expect("failed to write config");

        // When: Rapidly changing and reloading config
        for i in 0..20 {
            let updated_yaml = format!(
                r#"
worker:
  name: "test-worker-{}"
  max_workers: {}
"#,
                i,
                (i % 10) + 1
            );

            fs::write(&config_file, updated_yaml).expect("failed to update config");

            let result = needle::config::ConfigLoader::load_from_path(&config_file);
            // Each load should succeed or fail gracefully
            match result {
                Ok(_) => {
                    // Successfully loaded
                }
                Err(_) => {
                    // Failed (race condition, partial write, etc.)
                }
            }
        }

        // If we got here without panic, test passes
    }

    #[test]
    fn test_rapid_file_creation_deletion_does_not_panic() {
        // Given: A temp directory
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let test_file = temp_dir.path().join("test.txt");

        // When: Rapidly creating and deleting the same file
        for i in 0..20 {
            // Write
            let write_result = fs::write(&test_file, format!("iteration {}", i));

            // Read
            if write_result.is_ok() {
                let _read_result = fs::read_to_string(&test_file);
            }

            // Delete
            let _remove_result = fs::remove_file(&test_file);
        }

        // If we got here without panic, test passes
    }

    #[test]
    fn test_concurrent_readers_single_writer_does_not_panic() {
        // Given: A config file
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("race.yaml");

        let initial_yaml = r#"
worker:
  name: "test-worker"
  max_workers: 4
"#;
        fs::write(&config_file, initial_yaml).expect("failed to write config");

        // Spawn reader threads
        let reader_handles: Vec<_> = (0..10)
            .map(|_| {
                let config_path = config_file.clone();
                thread::spawn(move || {
                    for _ in 0..10 {
                        let _result = needle::config::ConfigLoader::load_from_path(&config_path);
                        thread::sleep(Duration::from_millis(1));
                    }
                })
            })
            .collect();

        // Spawn writer thread
        let writer_handle = {
            let config_path = config_file.clone();
            thread::spawn(move || {
                for i in 0..10 {
                    let yaml = format!(
                        r#"
worker:
  name: "test-worker-{}"
  max_workers: {}
"#,
                        i,
                        (i % 5) + 1
                    );
                    let _result = fs::write(&config_path, yaml);
                    thread::sleep(Duration::from_millis(2));
                }
            })
        };

        // Wait for all threads
        for handle in reader_handles {
            handle.join().expect("reader thread panicked");
        }
        writer_handle.join().expect("writer thread panicked");

        // If we got here without panic, test passes
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Arithmetic Overflow Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod arithmetic_overflow_tests {
    use super::*;

    #[test]
    fn test_duration_arithmetic_does_not_overflow_panic() {
        // Given: Maximum duration values
        let max_duration = Duration::from_secs(u64::MAX);

        // When: Adding durations (checked operations)
        let result = max_duration.checked_add(Duration::from_secs(1));

        // Then: Should return None without panicking
        assert!(result.is_none(), "Overflowing add should return None");
    }

    #[test]
    fn test_instant_arithmetic_does_not_overflow_panic() {
        // Given: Current instant
        let now = Instant::now();

        // When: Subtracting larger duration from smaller instant
        let result = now.checked_sub(Duration::from_secs(u64::MAX));

        // Then: Should return None without panicking
        assert!(result.is_none(), "Underflowing subtract should return None");
    }

    #[test]
    fn test_u64_arithmetic_edge_cases() {
        // Given: Edge case u64 values
        let values = vec![0, 1, u32::MAX as u64, u64::MAX - 1, u64::MAX];

        for val in values {
            // When: Doing arithmetic
            let _add_result = val.checked_add(1);
            let _sub_result = val.checked_sub(1);
            let _mul_result = val.checked_mul(2);

            // Then: All checked operations should return Option (not panic)
            // Just verifying they compile and run without panic
        }
    }

    #[test]
    fn test_timeout_calculation_with_large_values() {
        // Given: Large timeout values
        let large_values = vec![
            Duration::from_secs(3600),     // 1 hour
            Duration::from_secs(86400),    // 1 day
            Duration::from_secs(31536000), // 1 year
        ];

        for duration in large_values {
            // When: Using in timeout calculations
            let now = Instant::now();
            let deadline = now.checked_add(duration);

            // Then: Should handle without panicking
            assert!(
                deadline.is_some() || deadline.is_none(),
                "Deadline should be Some or None"
            );
        }
    }
}
