//! Validation gates: pre-closure verification commands.
//!
//! After an agent exits successfully (code 0), validation gates run configured
//! shell commands in the workspace directory. If any command fails, the bead
//! is released instead of having its closure accepted.
//!
//! Inspired by bg-gate (docs/research/bg-gate-validation.md).

use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::process::Command;

// ──────────────────────────────────────────────────────────────────────────────
// Types
// ──────────────────────────────────────────────────────────────────────────────

/// Result of running all validation gate commands.
#[derive(Debug)]
pub struct GateResult {
    /// Whether all gates passed.
    pub passed: bool,
    /// Details of any failures (empty when `passed` is true).
    pub failures: Vec<GateFailure>,
}

/// A single gate command that failed.
#[derive(Debug, Clone)]
pub struct GateFailure {
    /// The command that was run.
    pub command: String,
    /// Process exit code (None if killed by signal).
    pub exit_code: Option<i32>,
    /// Combined stderr output (truncated to a reasonable length).
    pub output: String,
}

// ──────────────────────────────────────────────────────────────────────────────
// ValidationGate
// ──────────────────────────────────────────────────────────────────────────────

/// Runs configured verification commands in a workspace directory.
pub struct ValidationGate {
    commands: Vec<String>,
    workspace: PathBuf,
}

/// Maximum bytes of stderr to capture per gate command.
const MAX_OUTPUT_BYTES: usize = 4096;

impl ValidationGate {
    /// Create a new validation gate.
    ///
    /// Returns `None` if `commands` is empty (no verification configured).
    pub fn new(commands: Vec<String>, workspace: PathBuf) -> Option<Self> {
        if commands.is_empty() {
            return None;
        }
        Some(ValidationGate {
            commands,
            workspace,
        })
    }

    /// Run all gate commands sequentially. Stops at the first failure.
    ///
    /// Each command is executed via `sh -c "{command}"` in the workspace
    /// directory. A command passes if its exit code is 0.
    pub async fn run(&self) -> Result<GateResult> {
        let mut failures = Vec::new();

        for cmd in &self.commands {
            tracing::info!(
                command = %cmd,
                workspace = %self.workspace.display(),
                "running validation gate"
            );

            match self.run_command(cmd).await {
                Ok(()) => {
                    tracing::info!(command = %cmd, "validation gate passed");
                }
                Err(failure) => {
                    tracing::warn!(
                        command = %cmd,
                        exit_code = ?failure.exit_code,
                        "validation gate failed"
                    );
                    failures.push(failure);
                    // Stop on first failure — no point running more gates.
                    break;
                }
            }
        }

        Ok(GateResult {
            passed: failures.is_empty(),
            failures,
        })
    }

    /// Run a single command. Returns `Ok(())` on exit 0, `Err(GateFailure)` otherwise.
    async fn run_command(&self, cmd: &str) -> std::result::Result<(), GateFailure> {
        let result = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(&self.workspace)
            .output()
            .await;

        match result {
            Ok(output) => {
                if output.status.success() {
                    Ok(())
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let truncated = truncate_output(&stderr, MAX_OUTPUT_BYTES);
                    Err(GateFailure {
                        command: cmd.to_string(),
                        exit_code: output.status.code(),
                        output: truncated,
                    })
                }
            }
            Err(e) => Err(GateFailure {
                command: cmd.to_string(),
                exit_code: None,
                output: format!("failed to execute command: {}", e),
            }),
        }
    }

    /// Workspace directory where commands run.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
}

/// Truncate output to at most `max_bytes`, adding an ellipsis if truncated.
fn truncate_output(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        let truncated = &s[..max_bytes];
        format!("{}... [truncated]", truncated)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_returns_none_for_empty_commands() {
        assert!(ValidationGate::new(vec![], PathBuf::from("/tmp")).is_none());
    }

    #[test]
    fn new_returns_some_for_nonempty_commands() {
        let gate = ValidationGate::new(vec!["echo ok".to_string()], PathBuf::from("/tmp"));
        assert!(gate.is_some());
    }

    #[tokio::test]
    async fn gate_passes_on_successful_command() {
        let gate = ValidationGate::new(vec!["true".to_string()], PathBuf::from("/tmp")).unwrap();

        let result = gate.run().await.unwrap();
        assert!(result.passed);
        assert!(result.failures.is_empty());
    }

    #[tokio::test]
    async fn gate_fails_on_failing_command() {
        let gate = ValidationGate::new(vec!["false".to_string()], PathBuf::from("/tmp")).unwrap();

        let result = gate.run().await.unwrap();
        assert!(!result.passed);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].command, "false");
        assert_eq!(result.failures[0].exit_code, Some(1));
    }

    #[tokio::test]
    async fn gate_stops_at_first_failure() {
        let gate = ValidationGate::new(
            vec![
                "true".to_string(),
                "false".to_string(),
                "echo should-not-run".to_string(),
            ],
            PathBuf::from("/tmp"),
        )
        .unwrap();

        let result = gate.run().await.unwrap();
        assert!(!result.passed);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].command, "false");
    }

    #[tokio::test]
    async fn gate_captures_stderr_output() {
        let gate = ValidationGate::new(
            vec!["echo 'error detail' >&2; exit 1".to_string()],
            PathBuf::from("/tmp"),
        )
        .unwrap();

        let result = gate.run().await.unwrap();
        assert!(!result.passed);
        assert!(result.failures[0].output.contains("error detail"));
    }

    #[tokio::test]
    async fn multiple_commands_all_pass() {
        let gate = ValidationGate::new(
            vec![
                "true".to_string(),
                "echo ok".to_string(),
                "true".to_string(),
            ],
            PathBuf::from("/tmp"),
        )
        .unwrap();

        let result = gate.run().await.unwrap();
        assert!(result.passed);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn truncate_output_short_string() {
        let s = "hello";
        assert_eq!(truncate_output(s, 100), "hello");
    }

    #[test]
    fn truncate_output_long_string() {
        let s = "a".repeat(200);
        let result = truncate_output(&s, 50);
        assert!(result.len() < 200);
        assert!(result.ends_with("... [truncated]"));
    }
}
