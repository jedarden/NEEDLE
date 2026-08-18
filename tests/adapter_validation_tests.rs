//! Adapter validation test infrastructure.
//!
//! This module provides comprehensive test infrastructure for adapter validation,
//! including isolated HOME setup helpers, workspace/tempdir cleanup fixtures, and
//! documented isolation patterns for adapter testing.
//!
//! # Testing Isolation Pattern for Adapter Validation
//!
//! Adapter validation tests spawn real `needle` subprocesses via the adapter system,
//! which means they can leak into the real user environment without proper isolation.
//! This module enforces the following isolation pattern:
//!
//! ## 1. HOME Isolation (Required)
//!
//! All adapter validation tests MUST isolate the HOME directory to prevent:
//! - Scanning of real user workspaces by the Explore strand
//! - Contamination of production bead stores
//! - Phantom bead creation in real repositories
//!
//! ### Pattern: Always Set HOME in Test Environment
//!
//! ```rust
//! fn test_adapter_something() {
//!     let temp_home = TempHome::new().unwrap();
//!
//!     // Isolate HOME for the spawned subprocess
//!     cmd.env("HOME", temp_home.path());
//!
//!     // Now the subprocess will only see test-controlled workspaces
//! }
//! ```
//!
//! ### Why This Matters
//!
//! The Explore strand (enabled by default) scans `workspace_root` (defaulting to `$HOME`)
//! for bead workspaces. Without isolation, a test's spawned binary will:
//! - Discover real repos under the test's worker ID
//! - Create test beads in production stores
//! - Leave orphaned state after test completion
//!
//! ## 2. Workspace/Tempdir Cleanup
//!
//! All test-created directories MUST be cleaned up, even if the test panics.
//! This module provides [`TempHome`] and [`TempWorkspace`] fixtures that auto-cleanup
//! on drop.
//!
//! ## 3. Process Guarding
//!
//! Long-running adapter processes MUST be wrapped in [`ProcessGuard`] to ensure
//! cleanup even on test panic. See `tests/process_guard.rs` for the implementation.
//!
//! # Module Structure
//!
//! - **Isolation fixtures**: [`TempHome`], [`TempWorkspace`]
//! - **Adapter helpers**: [`test_adapter_path`], [`create_test_adapter`]
//! - **Validation utilities**: [`validate_adapter_config`], [`check_adapter_binary`]
//! - **Process management**: Re-exports from `process_guard`
//!
//! # Example Usage
//!
//! ```rust
//! use adapter_validation_tests::*;
//!
//! fn test_claude_adapter_config() {
//!     let temp_home = TempHome::new().unwrap();
//!     let adapter = create_test_adapter("claude-sonnet");
//!
//!     let validation = validate_adapter_config(&adapter);
//!     assert!(validation.is_ok());
//! }
//! ```

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ────────────────────────────────────────────────────────────────────────────────
// Re-exports for convenience
// ────────────────────────────────────────────────────────────────────────────────

// Note: ProcessGuard is available in the tests/process_guard.rs module
// Tests that need process cleanup should use that module or implement their own guard

// ────────────────────────────────────────────────────────────────────────────────
// Isolation Fixtures
// ────────────────────────────────────────────────────────────────────────────────

/// Temporary HOME directory fixture with automatic cleanup.
///
/// This fixture creates a temporary directory that can be used as HOME for
/// adapter subprocess isolation. It automatically cleans up on drop.
///
/// # Purpose
///
/// Prevents adapter subprocesses from scanning real user workspaces and
/// contaminating production bead stores.
///
/// # Example
///
/// ```rust
/// let temp_home = TempHome::new()?;
/// cmd.env("HOME", temp_home.path());
/// // subprocess now sees only test-controlled workspaces
/// ```
#[derive(Debug)]
#[allow(dead_code)] // temp_dir field is used in Drop implementation
pub struct TempHome {
    /// The underlying temporary directory.
    temp_dir: TempDir,
    /// Path to the temporary HOME directory.
    home_path: PathBuf,
}

impl TempHome {
    /// Create a new temporary HOME directory.
    ///
    /// # Returns
    ///
    /// A new `TempHome` fixture with an isolated temporary directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the temporary directory cannot be created.
    pub fn new() -> Result<Self> {
        let temp_dir =
            TempDir::new().context("Failed to create temporary directory for HOME isolation")?;

        let home_path = temp_dir.path().to_path_buf();

        // Create standard HOME subdirectories
        fs::create_dir_all(home_path.join(".cache"))
            .context("Failed to create .cache directory in temp HOME")?;
        fs::create_dir_all(home_path.join(".config"))
            .context("Failed to create .config directory in temp HOME")?;

        Ok(Self {
            temp_dir,
            home_path,
        })
    }

    /// Get the path to the temporary HOME directory.
    ///
    /// # Returns
    ///
    /// A `PathBuf` pointing to the temporary HOME directory.
    pub fn path(&self) -> &Path {
        &self.home_path
    }

    /// Create a test workspace within this temporary HOME.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the workspace to create
    ///
    /// # Returns
    ///
    /// A `TempWorkspace` fixture contained within this HOME.
    pub fn create_workspace(&self, name: &str) -> Result<TempWorkspace> {
        TempWorkspace::new(self.path(), name)
    }

    /// Create a `.needle.yaml` configuration file in this HOME.
    ///
    /// # Arguments
    ///
    /// * `config` - The YAML configuration content to write
    ///
    /// # Returns
    ///
    /// The path to the created configuration file.
    pub fn create_needle_config(&self, config: &str) -> Result<PathBuf> {
        let config_path = self.path().join(".needle.yaml");
        let mut file = fs::File::create(&config_path)
            .with_context(|| format!("Failed to create .needle.yaml at {:?}", config_path))?;
        file.write_all(config.as_bytes())
            .context("Failed to write .needle.yaml configuration")?;
        Ok(config_path)
    }
}

/// Temporary workspace fixture with automatic cleanup.
///
/// This fixture creates a temporary workspace directory that can be used
/// for adapter testing. It automatically cleans up on drop.
///
/// # Purpose
///
/// Provides isolated workspace directories for testing adapter behavior
/// with different workspace configurations without affecting real repositories.
#[derive(Debug)]
#[allow(dead_code)] // Fields are used in Drop implementation
pub struct TempWorkspace {
    /// The underlying temporary directory (if owned by this fixture).
    temp_dir: Option<TempDir>,
    /// Path to the workspace directory.
    workspace_path: PathBuf,
}

impl TempWorkspace {
    /// Create a new temporary workspace within a parent directory.
    ///
    /// # Arguments
    ///
    /// * `parent` - The parent directory (typically a TempHome)
    /// * `name` - The name of the workspace to create
    ///
    /// # Returns
    ///
    /// A new `TempWorkspace` fixture.
    pub fn new(parent: &Path, name: &str) -> Result<Self> {
        let workspace_path = parent.join(name);

        // Create the workspace directory
        fs::create_dir_all(&workspace_path)
            .with_context(|| format!("Failed to create workspace at {:?}", workspace_path))?;

        // Initialize as a git repository (many adapter operations require git)
        let _output = std::process::Command::new("git")
            .arg("init")
            .current_dir(&workspace_path)
            .output()
            .context("Failed to initialize git repository in workspace")?;

        Ok(Self {
            temp_dir: None, // We don't own a temp dir - we're inside a parent
            workspace_path,
        })
    }

    /// Create a standalone temporary workspace with its own temp dir.
    ///
    /// # Returns
    ///
    /// A new `TempWorkspace` fixture with its own temporary directory.
    pub fn new_standalone() -> Result<Self> {
        let temp_dir =
            TempDir::new().context("Failed to create temporary directory for workspace")?;

        let workspace_path = temp_dir.path().join("workspace");

        fs::create_dir_all(&workspace_path)
            .with_context(|| format!("Failed to create workspace at {:?}", workspace_path))?;

        // Initialize as a git repository
        let _output = std::process::Command::new("git")
            .arg("init")
            .current_dir(&workspace_path)
            .output()
            .context("Failed to initialize git repository in workspace")?;

        Ok(Self {
            temp_dir: Some(temp_dir),
            workspace_path,
        })
    }

    /// Get the path to the workspace directory.
    pub fn path(&self) -> &Path {
        &self.workspace_path
    }

    /// Create a `.beads/` directory structure in this workspace.
    pub fn create_bead_store(&self) -> Result<PathBuf> {
        let beads_dir = self.workspace_path.join(".beads");
        fs::create_dir_all(&beads_dir)
            .with_context(|| format!("Failed to create .beads directory at {:?}", beads_dir))?;
        Ok(beads_dir)
    }

    /// Create a bead-forge configuration file in this workspace.
    pub fn create_bead_config(&self) -> Result<PathBuf> {
        let beads_dir = self.create_bead_store()?;
        let config_path = beads_dir.join("config.yaml");

        // Write a basic bead-forge configuration
        let config_content = r#"
# Bead forge configuration for testing
backend: bead-forge

# Test workspace configuration
workspaces:
  - path: .
"#;

        let mut file = fs::File::create(&config_path)
            .with_context(|| format!("Failed to create bead config at {:?}", config_path))?;
        file.write_all(config_content.as_bytes())
            .context("Failed to write bead configuration")?;

        Ok(config_path)
    }
}

// ────────────────────────────────────────────────────────────────────────────────
// Adapter Helper Functions
// ────────────────────────────────────────────────────────────────────────────────

/// Get the path to a built-in adapter YAML file.
///
/// # Arguments
///
/// * `adapter_name` - The name of the adapter (e.g., "claude-sonnet")
///
/// # Returns
///
/// A `PathBuf` pointing to the adapter's YAML file, if it exists.
pub fn test_adapter_path(adapter_name: &str) -> Option<PathBuf> {
    // Check both embedded and possible development locations
    let possible_paths = vec![
        // Embedded adapters (in the binary)
        PathBuf::from(format!(
            "/usr/local/share/needle/adapters/{}.yaml",
            adapter_name
        )),
        // Development location
        PathBuf::from(format!("../adapters/{}.yaml", adapter_name)),
        PathBuf::from(format!("adapters/{}.yaml", adapter_name)),
    ];

    possible_paths.into_iter().find(|path| path.exists())
}

/// Create a minimal test adapter configuration.
///
/// # Arguments
///
/// * `name` - The adapter name
///
/// # Returns
///
/// A YAML string representing a minimal adapter configuration.
pub fn create_test_adapter(name: &str) -> String {
    format!(
        r#"
# Test adapter: {name}
name: {name}
description: "Test adapter for validation testing"
agent_cli: test-agent
version_command: "test-agent --version"
input_method: stdin
invoke_template: 'echo "test prompt" | test-agent --model {{{{model}}}}'
timeout_secs: 120
idle_timeout_secs: 60
hard_timeout_secs: 300
provider: test
model: test-model
token_extraction:
  method: none
"#
    )
}

/// Create a test adapter with custom configuration.
///
/// # Arguments
///
/// * `name` - The adapter name
/// * `config_fn` - A function that modifies the base adapter configuration
///
/// # Returns
///
/// A YAML string representing the custom adapter configuration.
pub fn create_custom_adapter<F>(name: &str, config_fn: F) -> String
where
    F: FnOnce(&mut String),
{
    let mut config = create_test_adapter(name);
    config_fn(&mut config);
    config
}

// ────────────────────────────────────────────────────────────────────────────────
// Validation Utilities
// ────────────────────────────────────────────────────────────────────────────────

/// Validation result for adapter configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationResult {
    /// Whether the configuration is valid.
    pub is_valid: bool,
    /// List of validation errors (empty if valid).
    pub errors: Vec<String>,
    /// List of warnings (informational, not blocking).
    pub warnings: Vec<String>,
}

impl ValidationResult {
    /// Create a successful validation result.
    pub fn valid() -> Self {
        Self {
            is_valid: true,
            errors: vec![],
            warnings: vec![],
        }
    }

    /// Create a failed validation result.
    pub fn invalid(errors: Vec<String>) -> Self {
        Self {
            is_valid: false,
            errors,
            warnings: vec![],
        }
    }

    /// Add a warning to an existing validation result.
    pub fn with_warning(mut self, warning: String) -> Self {
        self.warnings.push(warning);
        self
    }

    /// Add multiple warnings to an existing validation result.
    pub fn with_warnings(mut self, warnings: Vec<String>) -> Self {
        self.warnings.extend(warnings);
        self
    }
}

/// Validate an adapter configuration YAML string.
///
/// # Arguments
///
/// * `adapter_yaml` - The adapter configuration YAML
///
/// # Returns
///
/// A `ValidationResult` indicating whether the configuration is valid.
pub fn validate_adapter_config(adapter_yaml: &str) -> ValidationResult {
    let mut errors = vec![];
    let mut warnings = vec![];

    // Check for required fields
    if !adapter_yaml.contains("name:") {
        errors.push("Missing required field: 'name'".to_string());
    }

    if !adapter_yaml.contains("invoke_template:") {
        errors.push("Missing required field: 'invoke_template'".to_string());
    }

    if !adapter_yaml.contains("input_method:") {
        errors.push("Missing required field: 'input_method'".to_string());
    }

    // Validate timeout values if present
    if let Some(timeout_caps) = extract_field_value(adapter_yaml, "timeout_secs") {
        if let Ok(timeout) = timeout_caps.parse::<u64>() {
            if timeout > 3600 {
                warnings.push(format!(
                    "timeout_secs ({}) exceeds 1 hour - this may be unusually long",
                    timeout
                ));
            }
        }
    }

    // Validate input method value
    if let Some(input_method) = extract_field_value(adapter_yaml, "input_method") {
        match input_method.trim() {
            "stdin" | "file" | "arg" => {}
            other => {
                errors.push(format!(
                    "Invalid input_method: '{}'. Must be one of: stdin, file, arg",
                    other
                ));
            }
        }
    }

    if errors.is_empty() {
        ValidationResult::valid().with_warnings(warnings)
    } else {
        ValidationResult::invalid(errors)
    }
}

/// Check if an adapter binary exists on PATH.
///
/// # Arguments
///
/// * `binary_name` - The name of the binary to check
///
/// # Returns
///
/// `true` if the binary exists on PATH, `false` otherwise.
pub fn check_adapter_binary(binary_name: &str) -> bool {
    let result = std::process::Command::new("which")
        .arg(binary_name)
        .output();

    match result {
        Ok(output) => output.status.success(),
        Err(_) => false,
    }
}

/// Get the version of an adapter binary.
///
/// # Arguments
///
/// * `binary_name` - The name of the binary
/// * `version_command` - Optional custom version command (defaults to `--version`)
///
/// # Returns
///
/// `Some(version_string)` if the version can be retrieved, `None` otherwise.
pub fn get_adapter_version(binary_name: &str, version_command: Option<&str>) -> Option<String> {
    let cmd = version_command.unwrap_or("--version");
    let parts = cmd.split_whitespace().collect::<Vec<_>>();

    let result = std::process::Command::new(binary_name)
        .args(&parts[1..]) // Skip the binary name itself if included
        .output();

    match result {
        Ok(output) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        _ => None,
    }
}

// ────────────────────────────────────────────────────────────────────────────────
// Helper Functions
// ────────────────────────────────────────────────────────────────────────────────

/// Extract the value of a field from YAML string.
///
/// This is a simple extraction that doesn't use a full YAML parser,
/// suitable for basic validation in tests.
fn extract_field_value(yaml: &str, field_name: &str) -> Option<String> {
    yaml.lines()
        .find(|line| line.trim().starts_with(field_name))
        .and_then(|line| line.split(':').nth(1).map(|value| value.trim().to_string()))
}

/// Create a minimal test prompt file for adapter invocation.
///
/// # Arguments
///
/// * `content` - The prompt content to write
///
/// # Returns
///
/// A `TempFile` fixture containing the prompt content.
pub fn create_test_prompt(content: &str) -> Result<TempFile> {
    TempFile::new_with_content("test-prompt.txt", content)
}

/// Temporary file fixture with automatic cleanup.
#[derive(Debug)]
#[allow(dead_code)] // temp_dir field is used in Drop implementation
pub struct TempFile {
    temp_dir: TempDir,
    file_path: PathBuf,
}

impl TempFile {
    /// Create a new temporary file with custom content.
    ///
    /// # Arguments
    ///
    /// * `filename` - The name of the file to create
    /// * `content` - The content to write to the file
    ///
    /// # Returns
    ///
    /// A new `TempFile` fixture.
    pub fn new_with_content(filename: &str, content: &str) -> Result<Self> {
        let temp_dir = TempDir::new().context("Failed to create temporary directory")?;

        let file_path = temp_dir.path().join(filename);
        let mut file = fs::File::create(&file_path)
            .with_context(|| format!("Failed to create file at {:?}", file_path))?;

        file.write_all(content.as_bytes())
            .context("Failed to write content to file")?;

        Ok(Self {
            temp_dir,
            file_path,
        })
    }

    /// Get the path to the temporary file.
    pub fn path(&self) -> &Path {
        &self.file_path
    }
}

// ────────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temp_home_creation() {
        let temp_home = TempHome::new().unwrap();

        // Verify HOME directory exists
        assert!(temp_home.path().exists());

        // Verify standard subdirectories exist
        assert!(temp_home.path().join(".cache").exists());
        assert!(temp_home.path().join(".config").exists());
    }

    #[test]
    fn test_temp_home_creates_workspace() {
        let temp_home = TempHome::new().unwrap();
        let workspace = temp_home.create_workspace("test-workspace").unwrap();

        assert!(workspace.path().exists());
        assert!(workspace.path().starts_with(temp_home.path()));
    }

    #[test]
    fn test_temp_workspace_creation() {
        let temp_home = TempHome::new().unwrap();
        let workspace = TempWorkspace::new(temp_home.path(), "test-ws").unwrap();

        assert!(workspace.path().exists());
        assert!(workspace.path().ends_with("test-ws"));
    }

    #[test]
    fn test_temp_workspace_standalone() {
        let workspace = TempWorkspace::new_standalone().unwrap();

        assert!(workspace.path().exists());
        assert!(workspace.path().join(".git").exists()); // Should be initialized as git repo
    }

    #[test]
    fn test_temp_workspace_creates_bead_store() {
        let temp_home = TempHome::new().unwrap();
        let workspace = temp_home.create_workspace("test-ws").unwrap();
        let beads_dir = workspace.create_bead_store().unwrap();

        assert!(beads_dir.exists());
        assert!(beads_dir.ends_with(".beads"));
    }

    #[test]
    fn test_create_test_adapter() {
        let adapter_yaml = create_test_adapter("test-adapter");

        assert!(adapter_yaml.contains("name: test-adapter"));
        assert!(adapter_yaml.contains("invoke_template:"));
        assert!(adapter_yaml.contains("input_method: stdin"));
    }

    #[test]
    fn test_create_custom_adapter() {
        let adapter_yaml = create_custom_adapter("custom-adapter", |config| {
            *config = config.replace("timeout_secs: 120", "timeout_secs: 300");
        });

        assert!(adapter_yaml.contains("name: custom-adapter"));
        assert!(adapter_yaml.contains("timeout_secs: 300"));
    }

    #[test]
    fn test_validate_adapter_config_valid() {
        let adapter_yaml = create_test_adapter("valid-adapter");
        let result = validate_adapter_config(&adapter_yaml);

        assert!(result.is_valid);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_validate_adapter_config_missing_name() {
        let adapter_yaml = r#"
description: "Adapter without name"
invoke_template: "echo test"
"#;
        let result = validate_adapter_config(adapter_yaml);

        assert!(!result.is_valid);
        assert!(result
            .errors
            .contains(&"Missing required field: 'name'".to_string()));
    }

    #[test]
    fn test_validate_adapter_config_missing_invoke_template() {
        let adapter_yaml = r#"
name: test-adapter
description: "Adapter without invoke template"
"#;
        let result = validate_adapter_config(adapter_yaml);

        assert!(!result.is_valid);
        assert!(result
            .errors
            .contains(&"Missing required field: 'invoke_template'".to_string()));
    }

    #[test]
    fn test_validate_adapter_config_invalid_input_method() {
        let adapter_yaml = r#"
name: test-adapter
input_method: invalid_method
invoke_template: "echo test"
"#;
        let result = validate_adapter_config(adapter_yaml);

        assert!(!result.is_valid);
        assert!(result
            .errors
            .iter()
            .any(|e| e.contains("Invalid input_method")));
    }

    #[test]
    fn test_validate_adapter_config_timeout_warning() {
        let adapter_yaml = r#"
name: test-adapter
input_method: stdin
invoke_template: "echo test"
timeout_secs: 7200
"#;
        let result = validate_adapter_config(adapter_yaml);

        assert!(result.is_valid); // Still valid, just a warning
        assert!(result.warnings.iter().any(|w| w.contains("unusually long")));
    }

    #[test]
    fn test_check_adapter_binary() {
        // 'sh' should exist on any Unix system
        assert!(check_adapter_binary("sh"));

        // 'nonexistent-binary-12345' should not exist
        assert!(!check_adapter_binary("nonexistent-binary-12345"));
    }

    #[test]
    fn test_get_adapter_version() {
        // Test with 'sh' which should have a version
        let version = get_adapter_version("sh", Some("--version"));

        // Version detection varies by shell, so we just check the function doesn't crash
        // Some shells don't support --version, so this might return None
        assert!(version.is_some() || version.is_none());
    }

    #[test]
    fn test_temp_file_creation() {
        let temp_file = create_test_prompt("Test prompt content").unwrap();

        assert!(temp_file.path().exists());
        assert!(temp_file.path().ends_with("test-prompt.txt"));

        // Verify content
        let content = fs::read_to_string(temp_file.path()).unwrap();
        assert_eq!(content, "Test prompt content");
    }

    #[test]
    fn test_temp_home_creates_needle_config() {
        let temp_home = TempHome::new().unwrap();
        let config_yaml = r#"
worker:
  name: test-worker
"#;
        let config_path = temp_home.create_needle_config(config_yaml).unwrap();

        assert!(config_path.exists());
        assert!(config_path.ends_with(".needle.yaml"));

        let content = fs::read_to_string(config_path).unwrap();
        assert!(content.contains("worker:"));
    }
}
