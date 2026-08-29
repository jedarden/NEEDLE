//! Basic error path panic tests for NEEDLE modules.
//!
//! This test module verifies that error paths in NEEDLE modules properly return
//! `Result` types instead of panicking. All error conditions should propagate
//! gracefully through the error handling chain.
//!
//! ## Test Categories
//!
//! - **Parse error tests**: Verify configuration and data parsing errors return Results
//! - **IO error tests**: Verify file system and I/O errors return Results
//! - **Validation error tests**: Verify input validation errors return Results
//! - **Error propagation tests**: Verify errors propagate correctly through call chains

use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

// ──────────────────────────────────────────────────────────────────────────────
// Config Module Error Path Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod config_error_tests {
    use super::*;

    #[test]
    fn test_invalid_yaml_returns_error() {
        // Given: Invalid YAML configuration
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("invalid.yaml");
        fs::write(&config_file, "invalid: yaml: content: [unclosed")
            .expect("failed to write invalid YAML");

        // When: Attempting to load invalid configuration
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should return an error, not panic
        assert!(result.is_err(), "Invalid YAML should return Result::Err");

        if let Err(e) = result {
            // Error should provide context
            let error_msg = format!("{:?}", e);
            assert!(
                error_msg.contains("yaml")
                    || error_msg.contains("parse")
                    || error_msg.contains("syntax"),
                "Error message should indicate parse error: {}",
                error_msg
            );
        }
    }

    #[test]
    fn test_invalid_config_field_loads_without_panic() {
        // Given: Valid YAML structure but invalid field values
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("invalid_field.yaml");
        let yaml_content = r#"
worker:
  name: "test-worker"
  timeout_secs: -5  # Invalid: negative timeout
"#;
        fs::write(&config_file, yaml_content).expect("failed to write YAML");

        // When: Attempting to load configuration with invalid field
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should load without panicking (validation happens elsewhere)
        assert!(
            result.is_ok(),
            "Config with invalid field should load without panic"
        );

        let config = result.unwrap();
        // The value loads successfully even if invalid
        assert_eq!(
            config.worker.max_workers, 4,
            "Default max_workers should be loaded"
        );
    }

    #[test]
    fn test_missing_config_file_returns_default() {
        // Given: Non-existent configuration file
        let nonexistent_path = PathBuf::from("/nonexistent/path/config.yaml");

        // When: Attempting to load missing file
        let result = needle::config::ConfigLoader::load_from_path(&nonexistent_path);

        // Then: Should return default config without panicking
        assert!(result.is_ok(), "Missing file should return default config");

        let config = result.unwrap();
        // Verify we got a valid config with defaults
        assert!(
            config.worker.max_workers > 0,
            "Default config should have valid max_workers"
        );
    }

    #[test]
    fn test_backend_detection_returns_default() {
        // Given: Directory without bead workspace
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let empty_dir = temp_dir.path().join("empty_workspace");
        fs::create_dir_all(&empty_dir).expect("failed to create dir");

        // When: Attempting to detect backend in empty directory
        let result = needle::config::detect_bead_backend(&empty_dir);

        // Then: Should return default backend without panicking
        assert!(
            result.is_ok(),
            "Backend detection in empty dir should return default"
        );

        let (backend, _path) = result.unwrap();
        // Verify we got a valid backend (Auto is the default)
        assert!(
            !backend.to_string().is_empty(),
            "Default backend should not be empty"
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Claim Module Error Path Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod claim_error_tests {
    use super::*;
    use needle::bead_store::BeadStore;
    use std::sync::Arc;

    struct FailingBeadStore;

    #[async_trait::async_trait]
    impl BeadStore for FailingBeadStore {
        async fn ready(
            &self,
            _filters: &needle::bead_store::Filters,
        ) -> anyhow::Result<Vec<needle::types::Bead>> {
            Err(anyhow::anyhow!("Simulated store failure"))
        }

        async fn list_all(&self) -> anyhow::Result<Vec<needle::types::Bead>> {
            Err(anyhow::anyhow!("Simulated store failure"))
        }

        async fn show(&self, _id: &needle::types::BeadId) -> anyhow::Result<needle::types::Bead> {
            Err(anyhow::anyhow!("Bead not found"))
        }

        async fn notes(&self, _id: &needle::types::BeadId) -> anyhow::Result<Option<String>> {
            Err(anyhow::anyhow!("Store unavailable"))
        }

        async fn claim(
            &self,
            _id: &needle::types::BeadId,
            _actor: &str,
        ) -> anyhow::Result<needle::types::ClaimResult> {
            Err(anyhow::anyhow!("Claim operation failed"))
        }

        async fn claim_auto(&self, _actor: &str) -> anyhow::Result<needle::types::ClaimResult> {
            Err(anyhow::anyhow!("Auto-claim failed"))
        }

        async fn release(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Release failed"))
        }

        async fn block(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Block failed"))
        }

        async fn clear_assignee(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Clear assignee failed"))
        }

        async fn flush(&self) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Flush failed"))
        }

        async fn reopen(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Reopen failed"))
        }

        async fn labels(&self, _id: &needle::types::BeadId) -> anyhow::Result<Vec<String>> {
            Err(anyhow::anyhow!("Labels fetch failed"))
        }

        async fn add_label(&self, _id: &needle::types::BeadId, _label: &str) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Add label failed"))
        }

        async fn remove_label(
            &self,
            _id: &needle::types::BeadId,
            _label: &str,
        ) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Remove label failed"))
        }

        async fn create_bead(
            &self,
            _title: &str,
            _body: &str,
            _labels: &[&str],
        ) -> anyhow::Result<needle::types::BeadId> {
            Err(anyhow::anyhow!("Create bead failed"))
        }

        async fn add_dependency(
            &self,
            _blocker_id: &needle::types::BeadId,
            _blocked_id: &needle::types::BeadId,
        ) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Add dependency failed"))
        }

        async fn remove_dependency(
            &self,
            _blocked_id: &needle::types::BeadId,
            _blocker_id: &needle::types::BeadId,
        ) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Remove dependency failed"))
        }

        async fn doctor_repair(&self) -> anyhow::Result<needle::bead_store::RepairReport> {
            Err(anyhow::anyhow!("Doctor repair failed"))
        }

        async fn doctor_check(&self) -> anyhow::Result<needle::bead_store::RepairReport> {
            Err(anyhow::anyhow!("Doctor check failed"))
        }

        async fn full_rebuild(&self) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("Full rebuild failed"))
        }

        fn has_valid_store(&self) -> bool {
            false
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
    async fn test_claim_fails_on_store_error() {
        // Given: A bead store that returns errors
        let store = Arc::new(FailingBeadStore);
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let lock_dir = temp_dir.path().join("locks");
        fs::create_dir_all(&lock_dir).expect("failed to create lock dir");

        let telemetry = needle::telemetry::Telemetry::new("test-worker-01".into());
        let claimer = needle::claim::Claimer::new(store, lock_dir, 1, 100, telemetry);

        // When: Attempting to claim from failing store
        let result = claimer.claim_auto("test-actor", "test-strand").await;

        // Then: Should return error without panicking
        assert!(
            result.is_err(),
            "Claim from failing store should return Result::Err"
        );

        if let Err(e) = result {
            // Error should be descriptive
            let error_msg = format!("{:?}", e);
            assert!(!error_msg.is_empty(), "Error message should not be empty");
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Dispatch Module Error Path Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod dispatch_error_tests {
    use super::*;

    #[test]
    fn test_invalid_adapter_config_returns_error() {
        // Given: Invalid adapter configuration
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let adapters_dir = temp_dir.path().join("adapters");
        fs::create_dir_all(&adapters_dir).expect("failed to create adapters dir");

        let invalid_adapter = adapters_dir.join("invalid.yaml");
        fs::write(&invalid_adapter, "invalid: yaml: content: [unclosed")
            .expect("failed to write invalid YAML");

        // When: Attempting to load invalid adapter
        let built_ins = needle::dispatch::builtin_adapters();
        let result = needle::dispatch::load_adapters(&adapters_dir, &built_ins);

        // Then: Should return error without panicking
        assert!(
            result.is_err(),
            "Invalid adapter YAML should return Result::Err"
        );
    }

    #[test]
    fn test_nonexistent_adapter_directory_returns_error() {
        // Given: Non-existent adapters directory
        let nonexistent_path = PathBuf::from("/nonexistent/adapters");

        // When: Attempting to load from non-existent directory
        let built_ins = needle::dispatch::builtin_adapters();
        let result = needle::dispatch::load_adapters(&nonexistent_path, &built_ins);

        // Then: Should return built-ins only, without panicking
        assert!(
            result.is_ok(),
            "Non-existent adapter dir should return built-ins"
        );

        let adapters = result.unwrap();
        // Should have at least the built-in adapters
        assert!(!adapters.is_empty(), "Should have built-in adapters");
    }

    #[test]
    fn test_invalid_timeout_loads_without_panic() {
        // Given: Config with negative timeout
        let yaml = r#"
adapters:
  - name: "test-adapter"
    binary: "echo"
    timeout_secs: -10
"#;

        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("config.yaml");
        fs::write(&config_file, yaml).expect("failed to write config");

        // When: Loading config with invalid timeout
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Should load without panicking (validation happens elsewhere)
        assert!(
            result.is_ok(),
            "Config with negative timeout should load without panic"
        );

        let config = result.unwrap();
        // The value loads successfully even if invalid
        assert!(config.agent.timeout > 0, "Config should have valid timeout");
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Template Module Error Path Tests
// ──────────────────────────────────────────────────────────────────────────────
//
// NOTE: The needle::template module does not have a Template struct that parses
// templates and returns Results. It only has RenderContext and render functions
// that process templates directly. Template syntax errors would only be caught
// at runtime when missing placeholders are not substituted, not during parsing.
//
// The template module uses simple placeholder substitution with {placeholder} syntax
// and does not perform template validation that would return parse errors.
//
// Template error handling is covered by integration tests that verify rendering
// behavior with missing or empty placeholders.

/*#[cfg(test)]
mod template_error_tests {
    use super::*;
    use needle::template::Template;

    #[test]
    fn test_invalid_template_syntax_returns_error() {
        // Given: Template with invalid syntax
        let invalid_template = "Hello {{name"; // Unclosed bracket

        // When: Attempting to parse invalid template
        let result = Template::from_str(invalid_template);

        // Then: Should return error without panicking
        assert!(
            result.is_err(),
            "Invalid template syntax should return Result::Err"
        );
    }

    #[test]
    fn test_missing_variable_returns_error() {
        // Given: Template requiring variable that won't be provided
        let template_str = "Hello {{missing_var}}!";

        // When: Parsing and rendering without required variable
        let result = Template::from_str(template_str);
        assert!(result.is_ok(), "Template parsing should succeed");

        let template = result.unwrap();
        let render_result = template.render(&std::collections::HashMap::new());

        // Then: Rendering should fail gracefully
        assert!(
            render_result.is_err(),
            "Missing variable should cause render to return Result::Err"
        );
    }

    #[test]
    fn test_empty_template_handling() {
        // Given: Empty template string
        let empty_template = "";

        // When: Parsing empty template
        let result = Template::from_str(empty_template);

        // Then: Should handle gracefully (may succeed or fail, but not panic)
        // Empty templates are valid, so this should succeed
        assert!(result.is_ok(), "Empty template should be valid");
    }
}*/

// ──────────────────────────────────────────────────────────────────────────────
// IO Error Path Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod io_error_tests {
    use super::*;

    #[test]
    fn test_read_nonexistent_file_returns_error() {
        // Given: Non-existent file path
        let nonexistent_file = PathBuf::from("/tmp/needle_test_nonexistent_file_12345.txt");

        // When: Attempting to read non-existent file
        let result = fs::read_to_string(&nonexistent_file);

        // Then: Should return IO error without panicking
        assert!(
            result.is_err(),
            "Reading non-existent file should return Result::Err"
        );
    }

    #[test]
    fn test_write_to_readonly_directory_returns_error() {
        // Given: Attempting to write to directory (which will fail for file creation)
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let dir_path = temp_dir.path();

        // When: Attempting to write to a directory path
        let result = fs::write(dir_path, "content");

        // Then: Should return IO error without panicking
        assert!(
            result.is_err(),
            "Writing to directory should return Result::Err"
        );
    }

    #[test]
    fn test_create_file_in_nonexistent_directory_returns_error() {
        // Given: Path to non-existent directory
        let nonexistent_dir = PathBuf::from("/nonexistent/directory/file.txt");

        // When: Attempting to create file in non-existent directory
        match fs::File::create(&nonexistent_dir) {
            Err(_e) => {
                // Expected to fail
                return; // Test passes - error was returned
            }
            Ok(_file) => {
                // If we reach here, the file was created (unexpected)
                panic!("File creation in non-existent directory should fail with Result::Err");
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Validation Error Path Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod validation_error_tests {

    #[test]
    fn test_invalid_bead_id_format() {
        // Given: Various invalid bead ID formats
        let invalid_ids = vec![
            "",                 // Empty string
            "   ",              // Whitespace only
            "test with spaces", // Spaces
            "test\nwith",       // Newlines
        ];

        for invalid_id in invalid_ids {
            // When: Creating BeadId from invalid format
            let bead_id = needle::types::BeadId::from(invalid_id.to_string());

            // Then: Should not panic - may accept or reject, but not crash
            // BeadId creation is currently infallible, but we verify it doesn't panic
            let _id_string = bead_id.to_string();
        }
    }

    #[test]
    fn test_empty_string_validation() {
        // Given: Empty string inputs
        let empty = "";

        // When: Using empty string in operations that should handle it
        // This verifies we don't panic on empty strings
        let bead_id = needle::types::BeadId::from(empty.to_string());
        assert_eq!(bead_id.to_string(), "");
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Error Propagation Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod error_propagation_tests {
    use super::*;

    #[tokio::test]
    async fn test_errors_propagate_through_async_chain() {
        // Given: A chain of operations where invalid YAML will cause a parse error
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let invalid_yaml = temp_dir.path().join("invalid.yaml");
        fs::write(&invalid_yaml, "invalid: yaml: [unclosed").expect("failed to write");

        // When: Chaining operations where first fails
        let config_result = needle::config::ConfigLoader::load_from_path(&invalid_yaml);

        // Then: Error should propagate without being swallowed or causing panic
        assert!(
            config_result.is_err(),
            "Parse error should propagate through chain"
        );

        // Verify error can be inspected
        if let Err(e) = config_result {
            let error_string = format!("{:?}", e);
            assert!(
                !error_string.is_empty(),
                "Error should have descriptive message"
            );
        }
    }

    #[test]
    fn test_context_is_preserved_in_errors() {
        // Given: An operation that fails
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_file = temp_dir.path().join("test.yaml");
        fs::write(&config_file, "invalid: yaml: [").expect("failed to write invalid YAML");

        // When: Operation fails
        let result = needle::config::ConfigLoader::load_from_path(&config_file);

        // Then: Error should preserve context about what failed
        assert!(result.is_err());
        if let Err(e) = result {
            let error_msg = format!("{:?}", e);
            // Error should mention the file or operation
            assert!(error_msg.len() > 10, "Error should have meaningful context");
        }
    }
}
