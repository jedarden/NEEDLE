//! Validation gates: pre-closure verification.
//!
//! After an agent exits successfully (code 0), validation gates run to verify
//! the work before accepting bead closure. If any gate fails, the bead is
//! released instead of having its closure accepted.
//!
//! # Pluggable Gate System
//!
//! Gates implement the [`Gate`] trait and are registered in the [`GateRegistry`].
//! Built-in gate types:
//! - `command`: Runs shell commands in the workspace directory
//!
//! Custom gates can be registered at runtime by calling [`GateRegistry::register`].
//!
//! Inspired by bg-gate (docs/research/bg-gate-validation.md).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::Result;
use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────────────
// Types
// ──────────────────────────────────────────────────────────────────────────────

/// Result of a single gate validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateResult {
    /// The gate passed validation.
    Pass,
    /// The gate failed validation with a reason.
    Fail(String),
}

impl GateResult {
    /// Returns true if the gate passed.
    pub fn passed(&self) -> bool {
        matches!(self, GateResult::Pass)
    }

    /// Returns the failure reason if this is a `Fail` result.
    pub fn failure_reason(&self) -> Option<&str> {
        match self {
            GateResult::Pass => None,
            GateResult::Fail(reason) => Some(reason),
        }
    }
}

/// Aggregated result of running multiple validation gates.
#[derive(Debug)]
pub struct GateReport {
    /// Whether all gates passed.
    pub all_passed: bool,
    /// Individual gate results keyed by gate name.
    pub results: HashMap<String, GateResult>,
}

impl GateReport {
    /// Create a new report from individual gate results.
    pub fn new(results: HashMap<String, GateResult>) -> Self {
        let all_passed = results.values().all(|r| r.passed());
        GateReport {
            all_passed,
            results,
        }
    }

    /// Create a report where all gates passed.
    pub fn all_pass() -> Self {
        GateReport {
            all_passed: true,
            results: HashMap::new(),
        }
    }

    /// Create a report with a single gate failure.
    pub fn single_failure(gate_name: impl Into<String>, reason: impl Into<String>) -> Self {
        let mut results = HashMap::new();
        results.insert(gate_name.into(), GateResult::Fail(reason.into()));
        GateReport {
            all_passed: false,
            results,
        }
    }
}

/// Configuration for a single gate from `.needle.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GateConfig {
    /// Run shell commands in the workspace directory.
    Command { commands: Vec<String> },
}

/// A single gate command that failed (for backwards compatibility).
#[derive(Debug, Clone)]
pub struct GateFailure {
    /// The command that was run.
    pub command: String,
    /// Process exit code (None if killed by signal).
    pub exit_code: Option<i32>,
    /// Combined stderr output (truncated to a reasonable length).
    pub output: String,
}

/// Aggregated result of running a `ValidationGate`.
#[derive(Debug)]
pub struct ValidationRunResult {
    /// Whether all gate commands passed.
    pub passed: bool,
    /// List of failures (empty when `passed` is true).
    pub failures: Vec<GateFailure>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Gate Trait
// ──────────────────────────────────────────────────────────────────────────────

/// A validation gate that can check bead work before accepting closure.
///
/// Gates are run after an agent exits successfully (code 0). If any gate
/// returns `GateResult::Fail`, the bead is released instead of having its
/// closure accepted.
pub trait Gate: Send + Sync {
    /// Validate the bead's work in the given workspace.
    ///
    /// Returns `GateResult::Pass` if validation succeeds, or `GateResult::Fail`
    /// with a human-readable reason if it fails.
    fn validate(&self, bead: &crate::types::Bead, workspace: &Path) -> Result<GateResult>;

    /// Gate type name for telemetry and configuration (e.g., "command", "custom").
    fn gate_type(&self) -> &str;
}

// ──────────────────────────────────────────────────────────────────────────────
// Gate Registry
// ──────────────────────────────────────────────────────────────────────────────

/// Registry for pluggable validation gates.
///
/// Gates are registered by type name. The registry is thread-safe and supports
/// dynamic registration of custom gate types.
pub struct GateRegistry {
    #[allow(clippy::type_complexity)]
    gates: RwLock<HashMap<String, Arc<dyn Fn(&GateConfig) -> Result<Arc<dyn Gate>> + Send + Sync>>>,
}

impl Default for GateRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl GateRegistry {
    /// Create a new registry with built-in gate types registered.
    pub fn new() -> Self {
        let registry = GateRegistry {
            gates: RwLock::new(HashMap::new()),
        };
        // Register built-in gate types
        registry.register_builtin_gates();
        registry
    }

    /// Register a built-in gate type constructor.
    fn register_builtin_gates(&self) {
        self.register("command", |config| match config {
            GateConfig::Command { commands } => Ok(Arc::new(CommandGate::new(commands.clone()))),
        });
    }

    /// Register a custom gate type constructor.
    ///
    /// The constructor function takes a `GateConfig` and returns a boxed `Gate`.
    /// Custom gates should parse their config from the `GateConfig` enum.
    ///
    /// # Example
    /// ```ignore
    /// registry.register("my_gate", |config| {
    ///     // Parse config and create custom gate
    ///     Ok(Arc::new(MyCustomGate::new(config)?))
    /// });
    /// ```
    pub fn register<F>(&self, gate_type: impl Into<String>, constructor: F)
    where
        F: Fn(&GateConfig) -> Result<Arc<dyn Gate>> + Send + Sync + 'static,
    {
        let gate_type = gate_type.into();
        let mut guards = self.gates.write().unwrap();
        guards.insert(gate_type, Arc::new(constructor));
    }

    /// Create a gate instance from its configuration.
    ///
    /// Returns an error if the gate type is not registered or construction fails.
    pub fn create_gate(&self, config: &GateConfig) -> Result<Arc<dyn Gate>> {
        let gate_type = match config {
            GateConfig::Command { .. } => "command",
        };

        let guards = self.gates.read().unwrap();
        let constructor = guards
            .get(gate_type)
            .ok_or_else(|| anyhow::anyhow!("unknown gate type '{}': not registered", gate_type))?;

        constructor(config)
    }

    /// Get the global gate registry instance.
    pub fn global() -> &'static GateRegistry {
        use std::sync::OnceLock;
        static REGISTRY: OnceLock<GateRegistry> = OnceLock::new();
        REGISTRY.get_or_init(GateRegistry::new)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Built-in Gate Types
// ──────────────────────────────────────────────────────────────────────────────

/// Runs configured shell commands as validation gates.
pub struct CommandGate {
    commands: Vec<String>,
}

impl CommandGate {
    /// Create a new command gate.
    pub fn new(commands: Vec<String>) -> Self {
        CommandGate { commands }
    }

    /// Maximum bytes of stderr to capture per gate command.
    const MAX_OUTPUT_BYTES: usize = 4096;
}

impl Gate for CommandGate {
    fn validate(&self, _bead: &crate::types::Bead, workspace: &Path) -> Result<GateResult> {
        // Run commands sequentially; stop at first failure.
        for cmd in &self.commands {
            tracing::info!(
                command = %cmd,
                workspace = %workspace.display(),
                "running command gate"
            );

            match self.run_command(cmd, workspace) {
                Ok(()) => {
                    tracing::info!(command = %cmd, "command gate passed");
                }
                Err(failure) => {
                    tracing::warn!(
                        command = %cmd,
                        exit_code = ?failure.exit_code,
                        "command gate failed"
                    );
                    return Ok(GateResult::Fail(format!(
                        "command '{}' failed: {}",
                        cmd,
                        failure.output.trim()
                    )));
                }
            }
        }

        Ok(GateResult::Pass)
    }

    fn gate_type(&self) -> &str {
        "command"
    }
}

impl CommandGate {
    /// Run a single command. Returns `Ok(())` on exit 0, `Err(GateFailure)` otherwise.
    fn run_command(&self, cmd: &str, workspace: &Path) -> std::result::Result<(), GateFailure> {
        let result = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(workspace)
            .output();

        match result {
            Ok(output) => {
                if output.status.success() {
                    Ok(())
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let truncated = truncate_output(&stderr, Self::MAX_OUTPUT_BYTES);
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
// ValidationGate (Main Entry Point)
// ──────────────────────────────────────────────────────────────────────────────

/// Runs configured verification gates in a workspace directory.
///
/// This is the main entry point for validation. It uses the pluggable gate
/// system to run all configured gates and returns an aggregated report.
pub struct ValidationGate {
    gates: Vec<(String, Arc<dyn Gate>)>,
    workspace: PathBuf,
}

impl ValidationGate {
    /// Create a new validation gate from gate configurations.
    ///
    /// Returns `None` if `gate_configs` is empty (no verification configured).
    pub fn new(gate_configs: Vec<(String, GateConfig)>, workspace: PathBuf) -> Option<Self> {
        if gate_configs.is_empty() {
            return None;
        }

        let registry = GateRegistry::global();
        let mut gates = Vec::new();

        for (name, config) in gate_configs {
            match registry.create_gate(&config) {
                Ok(gate) => gates.push((name, gate)),
                Err(e) => {
                    tracing::warn!(
                        gate_name = %name,
                        error = %e,
                        "failed to create gate — skipping"
                    );
                }
            }
        }

        if gates.is_empty() {
            return None;
        }

        Some(ValidationGate { gates, workspace })
    }

    /// Create from legacy command list (backwards compatibility).
    ///
    /// This method maintains the existing API for code that uses `Vec<String>`
    /// for verification commands.
    pub fn from_commands(commands: Vec<String>, workspace: PathBuf) -> Option<Self> {
        if commands.is_empty() {
            return None;
        }
        let gate = Arc::new(CommandGate::new(commands));
        Some(ValidationGate {
            gates: vec![("command_gate".to_string(), gate as Arc<dyn Gate>)],
            workspace,
        })
    }

    /// Run all gates sequentially. Stops at the first failure.
    pub async fn run(&self, bead: &crate::types::Bead) -> Result<GateReport> {
        let mut results = HashMap::new();

        for (name, gate) in &self.gates {
            let result = gate.validate(bead, &self.workspace)?;
            results.insert(name.clone(), result);

            // Stop on first failure.
            if !results.values().all(|r| r.passed()) {
                break;
            }
        }

        Ok(GateReport::new(results))
    }

    /// Workspace directory where gates run.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Bead, BeadId, BeadStatus};
    use chrono::Utc;

    fn test_bead() -> Bead {
        Bead {
            id: BeadId::from("needle-test"),
            title: "Test bead".to_string(),
            body: Some("Test body".to_string()),
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: Some("worker-01".to_string()),
            labels: vec![],
            workspace: PathBuf::from("/tmp"),
            dependencies: vec![],
            dependents: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ── GateResult tests ──

    #[test]
    fn gate_result_pass_returns_true() {
        assert!(GateResult::Pass.passed());
    }

    #[test]
    fn gate_result_fail_returns_false() {
        assert!(!GateResult::Fail("error".to_string()).passed());
    }

    #[test]
    fn gate_result_fail_has_reason() {
        let result = GateResult::Fail("test error".to_string());
        assert_eq!(result.failure_reason(), Some("test error"));
    }

    #[test]
    fn gate_result_pass_has_no_reason() {
        assert!(GateResult::Pass.failure_reason().is_none());
    }

    // ── GateReport tests ──

    #[test]
    fn gate_report_all_pass() {
        let report = GateReport::all_pass();
        assert!(report.all_passed);
        assert!(report.results.is_empty());
    }

    #[test]
    fn gate_report_single_failure() {
        let report = GateReport::single_failure("test_gate", "failed");
        assert!(!report.all_passed);
        assert_eq!(report.results.len(), 1);
        assert!(!report.results["test_gate"].passed());
    }

    #[test]
    fn gate_report_new_from_results() {
        let mut results = HashMap::new();
        results.insert("gate1".to_string(), GateResult::Pass);
        results.insert("gate2".to_string(), GateResult::Pass);
        let report = GateReport::new(results);
        assert!(report.all_passed);
        assert_eq!(report.results.len(), 2);
    }

    #[test]
    fn gate_report_new_with_failure() {
        let mut results = HashMap::new();
        results.insert("gate1".to_string(), GateResult::Pass);
        results.insert("gate2".to_string(), GateResult::Fail("error".to_string()));
        let report = GateReport::new(results);
        assert!(!report.all_passed);
    }

    // ── GateRegistry tests ──

    #[test]
    fn registry_global_returns_same_instance() {
        let r1 = GateRegistry::global();
        let r2 = GateRegistry::global();
        // Same pointer means same instance
        assert!(std::ptr::eq(r1, r2));
    }

    #[test]
    fn registry_creates_command_gate() {
        let registry = GateRegistry::global();
        let config = GateConfig::Command {
            commands: vec!["true".to_string()],
        };
        let gate = registry.create_gate(&config).unwrap();
        assert_eq!(gate.gate_type(), "command");
    }

    #[test]
    fn registry_fails_unknown_gate_type() {
        // We can't directly test unknown types since the registry uses string matching,
        // but we can verify the error path exists by using an invalid config variant
        // if we had one. For now, this test documents the expected behavior.
    }

    #[test]
    fn registry_register_custom_gate() {
        let registry = GateRegistry::new(); // Fresh registry

        // Register a test gate
        registry.register("test_gate", |_| Ok(Arc::new(TestGate)));

        // Verify we can create it (would need custom config variant)
        // This documents the registration API
    }

    // Test gate for registry testing
    struct TestGate;

    impl Gate for TestGate {
        fn validate(&self, _bead: &crate::types::Bead, _workspace: &Path) -> Result<GateResult> {
            Ok(GateResult::Pass)
        }

        fn gate_type(&self) -> &str {
            "test"
        }
    }

    // ── CommandGate tests ──

    #[test]
    fn command_gate_passes_on_true() {
        let gate = CommandGate::new(vec!["true".to_string()]);
        let bead = test_bead();
        let result = gate.validate(&bead, Path::new("/tmp")).unwrap();
        assert!(result.passed());
    }

    #[test]
    fn command_gate_fails_on_false() {
        let gate = CommandGate::new(vec!["false".to_string()]);
        let bead = test_bead();
        let result = gate.validate(&bead, Path::new("/tmp")).unwrap();
        assert!(!result.passed());
        assert!(result.failure_reason().unwrap().contains("failed"));
    }

    #[test]
    fn command_gate_stops_at_first_failure() {
        let gate = CommandGate::new(vec![
            "true".to_string(),
            "false".to_string(),
            "echo should-not-run".to_string(),
        ]);
        let bead = test_bead();
        let result = gate.validate(&bead, Path::new("/tmp")).unwrap();
        assert!(!result.passed());
        // Should be the false command that failed
        assert!(result.failure_reason().unwrap().contains("false"));
    }

    #[test]
    fn command_gate_type() {
        let gate = CommandGate::new(vec!["true".to_string()]);
        assert_eq!(gate.gate_type(), "command");
    }

    // ── ValidationGate tests ──

    #[test]
    fn validation_gate_new_returns_none_for_empty_configs() {
        let gate = ValidationGate::new(vec![], PathBuf::from("/tmp"));
        assert!(gate.is_none());
    }

    #[test]
    fn validation_gate_from_commands_returns_none_for_empty() {
        let gate = ValidationGate::from_commands(vec![], PathBuf::from("/tmp"));
        assert!(gate.is_none());
    }

    #[test]
    fn validation_gate_from_commands_returns_some_for_nonempty() {
        let gate = ValidationGate::from_commands(vec!["true".to_string()], PathBuf::from("/tmp"));
        assert!(gate.is_some());
    }

    #[tokio::test]
    async fn validation_gate_run_passes() {
        let gate =
            ValidationGate::from_commands(vec!["true".to_string()], PathBuf::from("/tmp")).unwrap();
        let bead = test_bead();
        let report = gate.run(&bead).await.unwrap();
        assert!(report.all_passed);
    }

    #[tokio::test]
    async fn validation_gate_run_fails() {
        let gate = ValidationGate::from_commands(vec!["false".to_string()], PathBuf::from("/tmp"))
            .unwrap();
        let bead = test_bead();
        let report = gate.run(&bead).await.unwrap();
        assert!(!report.all_passed);
    }

    #[tokio::test]
    async fn validation_gate_workspace() {
        let workspace = PathBuf::from("/test/workspace");
        let gate =
            ValidationGate::from_commands(vec!["true".to_string()], workspace.clone()).unwrap();
        assert_eq!(gate.workspace(), &workspace);
    }

    // ── truncate_output tests ──

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

    // ── GateConfig deserialization tests ──

    #[test]
    fn gate_config_command_deserialize() {
        let yaml = r#"
            type: command
            commands:
                - cargo test
                - cargo clippy
        "#;
        let config: GateConfig = serde_yaml::from_str(yaml).unwrap();
        match config {
            GateConfig::Command { commands } => {
                assert_eq!(commands, vec!["cargo test", "cargo clippy"]);
            }
        }
    }

    #[test]
    fn gate_config_command_serialize_roundtrip() {
        let config = GateConfig::Command {
            commands: vec!["echo test".to_string()],
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        let decoded: GateConfig = serde_yaml::from_str(&yaml).unwrap();
        match decoded {
            GateConfig::Command { commands } => {
                assert_eq!(commands, vec!["echo test"]);
            }
        }
    }
}
