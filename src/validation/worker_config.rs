//! Worker configuration validation.
//!
//! This module provides validation functions for worker configuration,
//! particularly for detecting unsafe configuration combinations that
//! could lead to operational issues.

use crate::types::IdleAction;

/// Validation result for worker configuration checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerConfigValidationResult {
    /// Configuration is valid and safe.
    Valid,
    /// Configuration is unsafe with a detailed reason.
    Invalid { reason: String },
}

impl WorkerConfigValidationResult {
    /// Returns true if the configuration is valid.
    pub fn is_valid(&self) -> bool {
        matches!(self, WorkerConfigValidationResult::Valid)
    }

    /// Returns the error reason if validation failed.
    pub fn error_reason(&self) -> Option<&str> {
        match self {
            WorkerConfigValidationResult::Valid => None,
            WorkerConfigValidationResult::Invalid { reason } => Some(reason),
        }
    }
}

/// Validate idle_action configuration against supervisor presence.
///
/// This function checks whether the configured `idle_action` is safe given
/// the presence or absence of a supervisor process. **Note:** This function
/// only validates — it does not mutate configuration. The worker boot code
/// handles the actual default-to-Wait behavior when appropriate.
///
/// # Safety Rule
///
/// `IdleAction::Exit` is **only safe when a supervisor is present**. Without
/// a supervisor, when a worker exits due to an empty queue, any in-progress
/// beads remain orphaned with no mechanism to reclaim them. The supervisor
/// solves this by spawning new workers that can claim and retry orphaned beads.
///
/// # Default Behavior
///
/// When no supervisor is detected and `idle_action=exit`, the worker **automatically
/// defaults to `wait`** unless `allow_exit_without_supervisor` is explicitly set
/// to `true`. This is a safety feature to prevent orphaned beads.
///
/// # Arguments
///
/// * `idle_action` - The configured idle action (Wait or Exit)
/// * `supervisor_present` - Whether a supervisor process was detected
///
/// # Returns
///
/// * `WorkerConfigValidationResult::Valid` - If configuration is safe
/// * `WorkerConfigValidationResult::Invalid` - If configuration is unsafe without explicit opt-in
///
/// # Examples
///
/// ```
/// use needle::types::IdleAction;
/// use needle::validation::worker_config::validate_idle_action_config;
///
/// // Safe: Exit with supervisor present
/// let result = validate_idle_action_config(IdleAction::Exit, true);
/// assert!(result.is_valid());
///
/// // Unsafe: Exit without supervisor (requires allow_exit_without_supervisor=true)
/// let result = validate_idle_action_config(IdleAction::Exit, false);
/// assert!(!result.is_valid());
/// assert!(result.error_reason().unwrap().contains("no supervisor"));
///
/// // Safe: Wait without supervisor (default, safe configuration)
/// let result = validate_idle_action_config(IdleAction::Wait, false);
/// assert!(result.is_valid());
///
/// // Safe: Wait with supervisor (valid but uncommon)
/// let result = validate_idle_action_config(IdleAction::Wait, true);
/// assert!(result.is_valid());
/// ```
pub fn validate_idle_action_config(
    idle_action: &IdleAction,
    supervisor_present: bool,
) -> WorkerConfigValidationResult {
    // Only Exit without supervisor is unsafe
    if *idle_action == IdleAction::Exit && !supervisor_present {
        return WorkerConfigValidationResult::Invalid {
            reason: "idle_action=exit configured without supervisor supervision: when the queue is empty, the worker will exit and leave any in-progress beads orphaned with no reclaim mechanism. This is the exact failure mode from the 2026-06-21 incident. Set idle_action=wait (safer: keeps worker alive to retry) or run workers under a supervisor (needle supervise) which can spawn replacements to reclaim orphaned beads.".to_string(),
        };
    }

    WorkerConfigValidationResult::Valid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_with_supervisor_is_valid() {
        let result = validate_idle_action_config(&IdleAction::Exit, true);
        assert!(result.is_valid());
        assert_eq!(result.error_reason(), None);
    }

    #[test]
    fn test_exit_without_supervisor_is_invalid() {
        let result = validate_idle_action_config(&IdleAction::Exit, false);
        assert!(!result.is_valid());
        let reason = result.error_reason().unwrap();
        assert!(reason.contains("without supervisor supervision"));
        assert!(reason.contains("orphaned"));
    }

    #[test]
    fn test_wait_without_supervisor_is_valid() {
        let result = validate_idle_action_config(&IdleAction::Wait, false);
        assert!(result.is_valid());
        assert_eq!(result.error_reason(), None);
    }

    #[test]
    fn test_wait_with_supervisor_is_valid() {
        let result = validate_idle_action_config(&IdleAction::Wait, true);
        assert!(result.is_valid());
        assert_eq!(result.error_reason(), None);
    }

    #[test]
    fn test_invalid_result_contains_helpful_message() {
        let result = validate_idle_action_config(&IdleAction::Exit, false);
        let reason = result.error_reason().unwrap();

        // Verify the error message is comprehensive and actionable
        assert!(reason.contains("idle_action=exit"));
        assert!(reason.contains("without supervisor supervision"));
        assert!(reason.contains("orphaned"));
        assert!(reason.contains("needle supervise"));
        assert!(reason.contains("idle_action=wait"));
    }

    #[test]
    fn test_validation_result_error_reason() {
        let valid = WorkerConfigValidationResult::Valid;
        assert_eq!(valid.error_reason(), None);

        let invalid = WorkerConfigValidationResult::Invalid {
            reason: "test error".to_string(),
        };
        assert_eq!(invalid.error_reason(), Some("test error"));
    }

    #[test]
    fn test_validation_result_is_valid() {
        assert!(WorkerConfigValidationResult::Valid.is_valid());
        assert!(!WorkerConfigValidationResult::Invalid {
            reason: "error".to_string(),
        }
        .is_valid());
    }

    #[test]
    fn test_wait_is_default_without_supervisor() {
        // The default IdleAction is Wait, which is safe without supervisor
        let default_action = IdleAction::default();
        assert_eq!(default_action, IdleAction::Wait);

        // Wait should always validate successfully
        let result = validate_idle_action_config(&default_action, false);
        assert!(result.is_valid());
        assert_eq!(result.error_reason(), None);
    }

    #[test]
    fn test_exit_without_supervisor_is_invalid_by_default() {
        // Exit without supervisor is unsafe (returns Invalid)
        let result = validate_idle_action_config(&IdleAction::Exit, false);
        assert!(!result.is_valid());
        let reason = result.error_reason().unwrap();
        assert!(reason.contains("without supervisor supervision"));
        assert!(reason.contains("orphaned"));
    }

    #[test]
    fn test_exit_with_supervisor_remains_valid() {
        // Exit with supervisor is safe
        let result = validate_idle_action_config(&IdleAction::Exit, true);
        assert!(result.is_valid());
        assert_eq!(result.error_reason(), None);
    }

    #[test]
    fn test_wait_is_always_valid() {
        // Wait is valid with or without supervisor
        let result_no_sup = validate_idle_action_config(&IdleAction::Wait, false);
        assert!(result_no_sup.is_valid());

        let result_with_sup = validate_idle_action_config(&IdleAction::Wait, true);
        assert!(result_with_sup.is_valid());
    }
}
