//! Gate command path validation.
//!
//! Validates that gate command paths exist before dispatch. This prevents
//! silent failures where a non-existent verification script causes every
//! dispatch to fail without any clear error message.
//!
//! # Problem
//!
//! Workspaces can carry `gates:` or `verification:` entries that reference
//! non-existent paths for months without detection. For example, spaxel and
//! mta-my-way carried `verification: [/home/coding/.needle/hooks/verify-changes.sh]`
//! from April to 2026-08-29; the file never existed on any host, and every
//! dispatch there failed verification with no clear indication of the root cause.
//!
//! # Solution
//!
//! This module validates gate command paths at two points:
//! 1. **`needle doctor`** - fails the health check with a clear error message
//! 2. **Worker boot** - logs a `gate.command_missing` warning if a path doesn't exist
//!
//! # Path Types
//!
//! - **Absolute paths** (e.g., `/home/coding/.needle/hooks/verify-changes.sh`)
//!   Checked for existence directly
//! - **Workspace-relative paths** (e.g., `.needle/hooks/verify-changes.sh`)
//!   Checked relative to the workspace directory
//! - **Commands resolved via $PATH** (e.g., `cargo`, `npm`, `pytest`)
//!   Checked using the `which` command
//!
//! # Shell Commands
//!
//! Shell commands that involve pipes, redirects, or shell builtins are skipped
//! in validation (they're not simple paths that can be checked).

use std::path::Path;

use super::GateConfig;

/// Validation result for gate command path checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatePathValidationResult {
    /// All command paths are valid.
    Valid,
    /// One or more command paths are invalid.
    Invalid {
        errors: Vec<GatePathValidationError>,
    },
}

impl GatePathValidationResult {
    /// Returns true if all command paths are valid.
    pub fn is_valid(&self) -> bool {
        matches!(self, GatePathValidationResult::Valid)
    }

    /// Returns the validation errors if validation failed.
    pub fn errors(&self) -> &[GatePathValidationError] {
        match self {
            GatePathValidationResult::Valid => &[],
            GatePathValidationResult::Invalid { errors } => errors,
        }
    }
}

/// A single gate command path validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatePathValidationError {
    /// The command string that failed validation.
    pub command: String,
    /// The specific path that doesn't exist (extracted from the command).
    pub path: String,
    /// Type of path (absolute, workspace-relative, or PATH-resolved).
    pub path_type: PathType,
    /// The config file where this gate was defined (if known).
    pub config_file: Option<String>,
}

/// Type of path being validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathType {
    /// Absolute path (e.g., `/home/coding/.needle/hooks/verify-changes.sh`).
    Absolute,
    /// Workspace-relative path (e.g., `.needle/hooks/verify-changes.sh`).
    WorkspaceRelative,
    /// Command resolved via $PATH (e.g., `cargo`, `pytest`).
    PathResolved,
}

impl std::fmt::Display for GatePathValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.config_file {
            Some(config_file) => {
                write!(
                    f,
                    "gate command '{command}' in {config_file}: path '{path}' ({path_type:?}) does not exist",
                    command = self.command,
                    config_file = config_file,
                    path = self.path,
                    path_type = self.path_type
                )
            }
            None => {
                write!(
                    f,
                    "gate command '{command}': path '{path}' ({path_type:?}) does not exist",
                    command = self.command,
                    path = self.path,
                    path_type = self.path_type
                )
            }
        }
    }
}

/// Validates gate command paths from both `gates:` and legacy `verification:` configs.
///
/// This function checks that:
/// - Absolute paths exist
/// - Workspace-relative paths exist relative to the workspace
/// - Commands in $PATH can be found with `which`
///
/// Complex shell commands (pipes, redirects, shell builtins) are skipped in validation.
///
/// # Arguments
///
/// * `gates` - Gate configurations from the `gates:` field
/// * `verification` - Legacy verification commands from the `verification:` field
/// * `workspace` - The workspace directory (for resolving relative paths)
/// * `config_file` - Optional path to the config file for error messages
///
/// # Returns
///
/// * `GatePathValidationResult::Valid` - All paths exist
/// * `GatePathValidationResult::Invalid` - One or more paths don't exist
pub fn validate_gate_command_paths(
    gates: &[crate::validation::GateConfig],
    verification: &[String],
    workspace: &Path,
    config_file: Option<&Path>,
) -> GatePathValidationResult {
    let mut errors = Vec::new();

    // Validate gates: entries
    for gate_config in gates {
        let GateConfig::Command { commands, .. } = gate_config;
        for command in commands {
            if let Some(err) = validate_command_path(command, workspace, config_file) {
                errors.push(err);
            }
        }
    }

    // Validate legacy verification: entries
    for command in verification {
        if let Some(err) = validate_command_path(command, workspace, config_file) {
            errors.push(err);
        }
    }

    if errors.is_empty() {
        GatePathValidationResult::Valid
    } else {
        GatePathValidationResult::Invalid { errors }
    }
}

/// Validates a single command path.
///
/// Returns `None` if the command is valid or cannot be validated (complex shell
/// command). Returns `Some(GatePathValidationError)` if the path doesn't exist.
fn validate_command_path(
    command: &str,
    workspace: &Path,
    config_file: Option<&Path>,
) -> Option<GatePathValidationError> {
    // Skip validation for complex shell commands
    if is_complex_shell_command(command) {
        return None;
    }

    // Extract the first token (command or path)
    let first_token = extract_first_token(command)?;
    let config_file_str = config_file.map(|p| p.display().to_string());

    // Check if it's an absolute path
    if first_token.starts_with('/') {
        if Path::new(&first_token).exists() {
            return None;
        }
        return Some(GatePathValidationError {
            command: command.to_string(),
            path: first_token,
            path_type: PathType::Absolute,
            config_file: config_file_str,
        });
    }

    // Check if it's a workspace-relative path (contains ./ or ../)
    if first_token.starts_with("./") || first_token.starts_with("../") {
        let full_path = workspace.join(&first_token);
        if full_path.exists() {
            return None;
        }
        return Some(GatePathValidationError {
            command: command.to_string(),
            path: first_token,
            path_type: PathType::WorkspaceRelative,
            config_file: config_file_str,
        });
    }

    // Check if it's a simple filename (might be in $PATH or workspace-relative)
    if !first_token.contains('/') {
        // First try workspace-relative
        let workspace_path = workspace.join(&first_token);
        if workspace_path.exists() {
            return None;
        }

        // Then try $PATH resolution
        if which_exists(&first_token) {
            return None;
        }

        // If neither exists, report as PATH-resolved (most likely intent)
        return Some(GatePathValidationError {
            command: command.to_string(),
            path: first_token,
            path_type: PathType::PathResolved,
            config_file: config_file_str,
        });
    }

    // If it contains / but doesn't start with / or ./ or ../, treat as workspace-relative
    let full_path = workspace.join(&first_token);
    if full_path.exists() {
        return None;
    }

    Some(GatePathValidationError {
        command: command.to_string(),
        path: first_token,
        path_type: PathType::WorkspaceRelative,
        config_file: config_file_str,
    })
}

/// Checks if a command is a complex shell command that should be skipped.
///
/// Complex commands include:
/// - Commands with pipes (|)
/// - Commands with redirects (>, <, >>)
/// - Commands with shell builtins (cd, export, etc.)
/// - Commands with command substitution ($(), backticks)
/// - Commands with logical operators (&&, ||, ;)
fn is_complex_shell_command(command: &str) -> bool {
    let command = command.trim();

    // Check for complex shell operators
    if command.contains('|')
        || command.contains('>')
        || command.contains('<')
        || command.contains("&&")
        || command.contains("||")
        || command.contains(';')
        || command.contains('$')
        || command.contains('`')
    {
        return true;
    }

    // Check for shell builtins that don't make sense as standalone path checks
    let first_token = match extract_first_token(command) {
        Some(token) => token,
        None => return false,
    };

    if matches!(
        first_token.as_str(),
        "cd" | "export" | "unset" | "shift" | "exit" | "return" | "break" | "continue"
    ) {
        return true;
    }

    // An explicit shell invocation carrying an inline script (`sh -c '...'`)
    // is a shell construct too: the thing after -c is a program, not a path
    // this module could resolve or check.
    let shell = matches!(
        first_token.rsplit('/').next().unwrap_or(&first_token),
        "sh" | "bash" | "zsh" | "dash" | "ksh"
    );
    shell && command.split_whitespace().any(|token| token == "-c")
}

/// Extracts the first token from a command string.
///
/// Returns `None` if the command is empty or only whitespace.
fn extract_first_token(command: &str) -> Option<String> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Split on whitespace and return the first token
    trimmed.split_whitespace().next().map(|s| s.to_string())
}

/// Checks if a command exists in $PATH using the `which` command.
fn which_exists(command: &str) -> bool {
    which::which(command).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn test_validate_absolute_path_exists() {
        let temp = TempDir::new().unwrap();
        let script_path = temp.path().join("test.sh");
        std::fs::write(&script_path, "#!/bin/sh\n").unwrap();

        let result = validate_command_path(
            &script_path.display().to_string(),
            temp.path(),
            Some(&PathBuf::from("/workspace/.needle.yaml")),
        );

        assert!(
            result.is_none(),
            "Absolute path that exists should return None"
        );
    }

    #[test]
    fn test_validate_absolute_path_missing() {
        let temp = TempDir::new().unwrap();
        let missing_path = "/tmp/nonexistent_script_xyz123.sh";

        let result = validate_command_path(
            missing_path,
            temp.path(),
            Some(&PathBuf::from("/workspace/.needle.yaml")),
        );

        assert!(result.is_some());
        let err = result.unwrap();
        assert_eq!(err.path, missing_path);
        assert!(matches!(err.path_type, PathType::Absolute));
        assert!(err.command.contains(missing_path));
    }

    #[test]
    fn test_validate_workspace_relative_path_exists() {
        let temp = TempDir::new().unwrap();
        let script_path = temp.path().join(".needle/hooks/test.sh");
        std::fs::create_dir_all(script_path.parent().unwrap()).unwrap();
        std::fs::write(&script_path, "#!/bin/sh\n").unwrap();

        let result = validate_command_path(
            ".needle/hooks/test.sh",
            temp.path(),
            Some(&PathBuf::from("/workspace/.needle.yaml")),
        );

        assert!(
            result.is_none(),
            "Workspace-relative path that exists should return None"
        );
    }

    #[test]
    fn test_validate_workspace_relative_path_missing() {
        let temp = TempDir::new().unwrap();
        let missing_relative = ".needle/hooks/missing.sh";

        let result = validate_command_path(
            missing_relative,
            temp.path(),
            Some(&PathBuf::from("/workspace/.needle.yaml")),
        );

        assert!(result.is_some());
        let err = result.unwrap();
        assert_eq!(err.path, missing_relative);
        assert!(matches!(err.path_type, PathType::WorkspaceRelative));
    }

    #[test]
    fn test_validate_path_command_exists() {
        // `true` should always exist in $PATH
        let result = validate_command_path(
            "true",
            Path::new("/tmp"),
            Some(&PathBuf::from("/workspace/.needle.yaml")),
        );

        assert!(
            result.is_none(),
            "Command in PATH (true) should return None"
        );
    }

    #[test]
    fn test_validate_path_command_missing() {
        let result = validate_command_path(
            "nonexistent_command_xyz123",
            Path::new("/tmp"),
            Some(&PathBuf::from("/workspace/.needle.yaml")),
        );

        assert!(result.is_some());
        let err = result.unwrap();
        assert_eq!(err.path, "nonexistent_command_xyz123");
        assert!(matches!(err.path_type, PathType::PathResolved));
    }

    #[test]
    fn test_skip_complex_shell_commands() {
        let complex_commands = vec![
            "cargo test | grep foo",
            "npm build > /tmp/build.log",
            "make && make install",
            "sh -c 'echo test'",
            "cat file | grep pattern",
            "./script.sh; echo done",
            "cd /tmp && ls",
            "export FOO=bar",
        ];

        for command in complex_commands {
            let result = validate_command_path(
                command,
                Path::new("/tmp"),
                Some(&PathBuf::from("/workspace/.needle.yaml")),
            );
            assert!(
                result.is_none(),
                "Complex command '{}' should be skipped (return None)",
                command
            );
        }
    }

    #[test]
    fn test_validate_gate_command_paths_all_valid() {
        let temp = TempDir::new().unwrap();
        let script_path = temp.path().join("test.sh");
        std::fs::write(&script_path, "#!/bin/sh\n").unwrap();

        let gates = vec![crate::validation::GateConfig::Command {
            commands: vec![script_path.display().to_string(), "true".to_string()],
            stderr_cap_bytes: None,
            run_in: crate::validation::RunIn::Clean,
        }];

        let result = validate_gate_command_paths(&gates, &[], temp.path(), None);

        assert!(result.is_valid());
    }

    #[test]
    fn test_validate_gate_command_paths_with_errors() {
        let temp = TempDir::new().unwrap();
        let script_path = temp.path().join("exists.sh");
        std::fs::write(&script_path, "#!/bin/sh\n").unwrap();

        let gates = vec![crate::validation::GateConfig::Command {
            commands: vec![
                script_path.display().to_string(),
                "/nonexistent/path.sh".to_string(),
                "nonexistent_command_xyz".to_string(),
            ],
            stderr_cap_bytes: None,
            run_in: crate::validation::RunIn::Clean,
        }];

        let result = validate_gate_command_paths(&gates, &[], temp.path(), None);

        assert!(!result.is_valid());
        let errors = result.errors();
        assert_eq!(errors.len(), 2);

        // First error should be the absolute path
        assert_eq!(errors[0].path, "/nonexistent/path.sh");
        assert!(matches!(errors[0].path_type, PathType::Absolute));

        // Second error should be the PATH command
        assert_eq!(errors[1].path, "nonexistent_command_xyz");
        assert!(matches!(errors[1].path_type, PathType::PathResolved));
    }

    #[test]
    fn test_validate_gate_command_paths_legacy_verification() {
        let temp = TempDir::new().unwrap();
        let script_path = temp.path().join("verify.sh");
        std::fs::write(&script_path, "#!/bin/sh\n").unwrap();

        let verification = vec![
            script_path.display().to_string(),
            "/nonexistent/verify.sh".to_string(),
        ];

        let result = validate_gate_command_paths(&[], &verification, temp.path(), None);

        assert!(!result.is_valid());
        let errors = result.errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path, "/nonexistent/verify.sh");
    }

    #[test]
    fn test_extract_first_token() {
        assert_eq!(extract_first_token("cargo test"), Some("cargo".to_string()));
        assert_eq!(
            extract_first_token("  /path/to/script.sh  "),
            Some("/path/to/script.sh".to_string())
        );
        assert_eq!(
            extract_first_token(".needle/hooks/verify.sh"),
            Some(".needle/hooks/verify.sh".to_string())
        );
        assert_eq!(extract_first_token(""), None);
        assert_eq!(extract_first_token("   "), None);
    }

    #[test]
    fn test_is_complex_shell_command() {
        assert!(is_complex_shell_command("cargo test | grep foo"));
        assert!(is_complex_shell_command("npm build > /tmp/log"));
        assert!(is_complex_shell_command("make && make install"));
        assert!(is_complex_shell_command("sh -c 'echo test'"));
        assert!(is_complex_shell_command("cd /tmp && ls"));
        assert!(is_complex_shell_command("export FOO=bar"));

        assert!(!is_complex_shell_command("cargo test"));
        assert!(!is_complex_shell_command("/path/to/script.sh"));
        assert!(!is_complex_shell_command("true"));
        assert!(!is_complex_shell_command(".needle/hooks/verify.sh"));
    }

    #[test]
    fn test_gate_path_validation_error_display() {
        let err = GatePathValidationError {
            command: "cargo test".to_string(),
            path: "/nonexistent/cargo".to_string(),
            path_type: PathType::Absolute,
            config_file: Some("/workspace/.needle.yaml".to_string()),
        };

        let display = format!("{}", err);
        assert!(display.contains("cargo test"));
        assert!(display.contains("/nonexistent/cargo"));
        assert!(display.contains("/workspace/.needle.yaml"));
    }

    #[test]
    fn test_gate_path_validation_error_display_no_config() {
        let err = GatePathValidationError {
            command: "npm test".to_string(),
            path: "npm".to_string(),
            path_type: PathType::PathResolved,
            config_file: None,
        };

        let display = format!("{}", err);
        assert!(display.contains("npm test"));
        assert!(display.contains("npm"));
        assert!(!display.contains(".needle.yaml"));
    }

    #[test]
    fn test_validation_result_is_valid() {
        let valid = GatePathValidationResult::Valid;
        assert!(valid.is_valid());
        assert_eq!(valid.errors(), &[]);
    }

    #[test]
    fn test_validation_result_invalid() {
        let errors = vec![GatePathValidationError {
            command: "test".to_string(),
            path: "/missing".to_string(),
            path_type: PathType::Absolute,
            config_file: None,
        }];

        let invalid = GatePathValidationResult::Invalid { errors };
        assert!(!invalid.is_valid());
        assert_eq!(invalid.errors().len(), 1);
    }
}
