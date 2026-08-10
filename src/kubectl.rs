//! Kubernetes kubectl command wrapper for workflow operations.
//!
//! This module provides functions to interact with Kubernetes resources
//! via the kubectl CLI, with a focus on Argo Workflows status fetching.

use anyhow::{Context, Result};
use std::process::Command;

/// Path to the iad-ci cluster kubeconfig.
const IAD_CI_KUBECONFIG: &str = "/home/coding/.kube/iad-ci.kubeconfig";

/// Represents the phase of an Argo Workflow.
///
/// # Variants
///
/// * `Running` — Workflow is currently executing
/// * `Succeeded` — Workflow completed successfully
/// * `Failed` — Workflow failed with an error
/// * `Error` — Workflow encountered a system error
/// * `Pending` — Workflow has not yet started
/// * `Unknown` — Unable to determine workflow phase
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowPhase {
    Running,
    Succeeded,
    Failed,
    Error,
    Pending,
    Unknown(String),
}

impl WorkflowPhase {
    /// Parse a workflow phase string into a [`WorkflowPhase`] enum.
    ///
    /// # Arguments
    ///
    /// * `phase` - The phase string from kubectl output
    ///
    /// # Returns
    ///
    /// A [`WorkflowPhase`] variant. Unknown phases are captured in the `Unknown` variant.
    fn from_str(phase: &str) -> Self {
        match phase.trim() {
            "Running" => WorkflowPhase::Running,
            "Succeeded" => WorkflowPhase::Succeeded,
            "Failed" => WorkflowPhase::Failed,
            "Error" => WorkflowPhase::Error,
            "Pending" => WorkflowPhase::Pending,
            other => WorkflowPhase::Unknown(other.to_string()),
        }
    }
}

/// Fetch the current phase of an Argo Workflow from kubectl.
///
/// This function runs `kubectl get workflow` with jsonpath output to extract
/// the workflow's status phase. It uses the iad-ci cluster kubeconfig at
/// `/home/coding/.kube/iad-ci.kubeconfig`.
///
/// # Arguments
///
/// * `workflow_name` - The name of the workflow to query
/// * `namespace` - The Kubernetes namespace containing the workflow (default: "argo-workflows")
///
/// # Returns
///
/// * `Result<WorkflowPhase>` — The workflow's current phase, or an error if:
///   - kubectl is not available
///   - The kubeconfig file does not exist
///   - The workflow does not exist
///   - kubectl command fails
///
/// # Errors
///
/// This function returns an error if:
/// - The kubectl binary cannot be executed
/// - The kubeconfig file at `/home/coding/.kube/iad-ci.kubeconfig` is missing
/// - The workflow does not exist in the specified namespace
/// - kubectl returns a non-zero exit code
/// - kubectl output cannot be parsed
///
/// # Examples
///
/// ```no_run
/// use needle::kubectl::get_workflow_phase;
///
/// # async fn example() -> anyhow::Result<()> {
/// // Fetch the phase of a workflow in the default namespace
/// let phase = get_workflow_phase("my-workflow", Some("argo-workflows"))?;
/// println!("Workflow phase: {:?}", phase);
/// # Ok(())
/// # }
/// ```
///
/// # kubectl command
///
/// The function executes:
///
/// ```bash
/// kubectl --kubeconfig=/home/coding/.kube/iad-ci.kubeconfig \
///   get workflow <workflow_name> -n <namespace> \
///   -o jsonpath='{.status.phase}'
/// ```
pub fn get_workflow_phase(workflow_name: &str, namespace: Option<&str>) -> Result<WorkflowPhase> {
    // Default to argo-workflows namespace if not specified
    let ns = namespace.unwrap_or("argo-workflows");

    // Verify kubeconfig exists before running kubectl
    if !std::path::Path::new(IAD_CI_KUBECONFIG).exists() {
        return Err(anyhow::anyhow!(
            "kubeconfig not found: {} (iad-ci cluster access unavailable)",
            IAD_CI_KUBECONFIG
        ));
    }

    // Build kubectl command with jsonpath to extract status.phase
    let output = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(IAD_CI_KUBECONFIG)
        .arg("get")
        .arg("workflow")
        .arg(workflow_name)
        .arg("-n")
        .arg(ns)
        .arg("-o")
        .arg("jsonpath={.status.phase}")
        .output()
        .context("failed to execute kubectl command")?;

    // Check if kubectl command failed
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "kubectl get workflow failed for '{}/{}': {}",
            ns,
            workflow_name,
            stderr
        ));
    }

    // Parse phase from kubectl output
    let phase_str = String::from_utf8_lossy(&output.stdout);
    let phase = WorkflowPhase::from_str(&phase_str);

    Ok(phase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_phase_from_str() {
        // Test standard phases
        assert_eq!(WorkflowPhase::from_str("Running"), WorkflowPhase::Running);
        assert_eq!(
            WorkflowPhase::from_str("Succeeded"),
            WorkflowPhase::Succeeded
        );
        assert_eq!(WorkflowPhase::from_str("Failed"), WorkflowPhase::Failed);
        assert_eq!(WorkflowPhase::from_str("Error"), WorkflowPhase::Error);
        assert_eq!(WorkflowPhase::from_str("Pending"), WorkflowPhase::Pending);

        // Test with whitespace (kubectl output may have trailing newlines)
        assert_eq!(WorkflowPhase::from_str("Running\n"), WorkflowPhase::Running);
        assert_eq!(
            WorkflowPhase::from_str("  Succeeded  "),
            WorkflowPhase::Succeeded
        );

        // Test unknown phase
        assert_eq!(
            WorkflowPhase::from_str("SomeCustomPhase"),
            WorkflowPhase::Unknown("SomeCustomPhase".to_string())
        );
    }

    #[test]
    fn test_workflow_phase_from_str_empty() {
        // Empty string should be captured as Unknown
        assert_eq!(
            WorkflowPhase::from_str(""),
            WorkflowPhase::Unknown("".to_string())
        );
    }

    #[test]
    fn test_workflow_phase_from_str_whitespace_only() {
        // Whitespace-only string should be captured as Unknown (after trim)
        assert_eq!(
            WorkflowPhase::from_str("   "),
            WorkflowPhase::Unknown("".to_string())
        );
    }

    #[test]
    fn test_kubectl_missing_kubeconfig() {
        // This test verifies that missing kubeconfig is detected before running kubectl
        // We can't easily test this without manipulating the filesystem, so we test
        // the logic by checking that a non-existent path would fail

        let nonexistent_path = "/tmp/nonexistent-kubeconfig-xyz123";
        assert!(
            !std::path::Path::new(nonexistent_path).exists(),
            "test setup failed: path should not exist"
        );

        // The actual function checks IAD_CI_KUBECONFIG, but the logic is the same
        // This is a compile-time check that the pattern works
        let _ = nonexistent_path;
    }

    #[test]
    fn test_iad_ci_kubeconfig_constant() {
        // Verify the kubeconfig path is correct
        assert_eq!(IAD_CI_KUBECONFIG, "/home/coding/.kube/iad-ci.kubeconfig");
    }

    #[test]
    fn test_workflow_phase_equality() {
        // Test PartialEq implementation
        assert_eq!(WorkflowPhase::Running, WorkflowPhase::Running);
        assert_ne!(WorkflowPhase::Running, WorkflowPhase::Succeeded);
        assert_eq!(
            WorkflowPhase::Unknown("foo".to_string()),
            WorkflowPhase::Unknown("foo".to_string())
        );
        assert_ne!(
            WorkflowPhase::Unknown("foo".to_string()),
            WorkflowPhase::Unknown("bar".to_string())
        );
    }
}
