//! Template rendering with placeholder substitution.
//!
//! Provides a generic template rendering system that substitutes placeholders
//! with actual values from bead context. This module is used by various strands
//! and components that need to render templates with bead data.
//!
//! ## Placeholders
//!
//! Templates use `{placeholder}` syntax. Supported placeholders:
//!
//! - `{bead_id}` - Bead identifier
//! - `{bead_title}` - Bead title
//! - `{bead_body}` - Bead body/description
//! - `{bead_status}` - Bead status (open, in_progress, done, blocked, deferred)
//! - `{bead_priority}` - Bead priority (1-4)
//! - `{bead_assignee}` - Current assignee (if any)
//! - `{bead_labels}` - Comma-separated labels
//! - `{workspace}` - Workspace path
//! - `{worker_id}` - Worker identifier
//! - `{created_at}` - Creation timestamp
//! - `{updated_at}` - Last update timestamp
//! - `{actor}` - Actor/assignee (alias for bead_assignee)
//!
//! ## Usage
//!
//! ```no_run
//! use needle::template::{RenderContext, render};
//! use needle::types::Bead;
//!
//! let bead = /* ... */;
//! let context = RenderContext::from_bead(&bead, "/path/to/workspace", "worker-01");
//! let template = "Work on {bead_title} in {workspace}";
//! let rendered = render(template, &context);
//! // Result: "Work on Implement feature X in /path/to/workspace"
//! ```

use crate::types::Bead;
use std::collections::HashMap;
use std::path::Path;

/// Context data for template rendering.
///
/// Holds all the bead and runtime data that can be substituted into templates.
#[derive(Debug, Clone)]
pub struct RenderContext {
    /// Bead identifier
    pub bead_id: String,
    /// Bead title
    pub bead_title: String,
    /// Bead body/description
    pub bead_body: Option<String>,
    /// Bead status
    pub bead_status: String,
    /// Bead priority (1-4, lower is higher priority)
    pub bead_priority: u8,
    /// Current assignee (if any)
    pub bead_assignee: Option<String>,
    /// Comma-separated labels
    pub bead_labels: String,
    /// Workspace path
    pub workspace: String,
    /// Worker identifier
    pub worker_id: String,
    /// Creation timestamp
    pub created_at: String,
    /// Last update timestamp
    pub updated_at: String,
    /// Actor/assignee (alias for bead_assignee)
    pub actor: Option<String>,
}

impl RenderContext {
    /// Create a RenderContext from a bead and runtime information.
    ///
    /// # Arguments
    ///
    /// * `bead` - The bead to render context from
    /// * `workspace` - Workspace path
    /// * `worker_id` - Worker identifier
    pub fn from_bead(bead: &Bead, workspace: &Path, worker_id: &str) -> Self {
        let labels = if bead.labels.is_empty() {
            String::new()
        } else {
            bead.labels.join(", ")
        };

        let created_at = bead.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();
        let updated_at = bead.updated_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();

        RenderContext {
            bead_id: bead.id.to_string(),
            bead_title: bead.title.clone(),
            bead_body: bead.body.clone(),
            bead_status: bead.status.to_string(),
            bead_priority: bead.priority,
            bead_assignee: bead.assignee.clone(),
            bead_labels: labels,
            workspace: workspace.display().to_string(),
            worker_id: worker_id.to_string(),
            created_at,
            updated_at,
            actor: bead.assignee.clone(),
        }
    }

    /// Create a RenderContext with custom values.
    ///
    /// This allows creating a context with arbitrary values for testing or
    /// special cases where you don't have a full bead.
    pub fn new() -> Self {
        RenderContext {
            bead_id: String::new(),
            bead_title: String::new(),
            bead_body: None,
            bead_status: "open".to_string(),
            bead_priority: 2,
            bead_assignee: None,
            bead_labels: String::new(),
            workspace: String::new(),
            worker_id: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            actor: None,
        }
    }

    /// Convert to a map of placeholder names to values.
    ///
    /// Returns a HashMap where keys are placeholder names without braces
    /// (e.g., "bead_id") and values are the corresponding string values.
    fn to_map(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();

        map.insert("bead_id".to_string(), self.bead_id.clone());
        map.insert("bead_title".to_string(), self.bead_title.clone());
        map.insert(
            "bead_body".to_string(),
            self.bead_body
                .clone()
                .unwrap_or_else(|| "(no description)".to_string()),
        );
        map.insert("bead_status".to_string(), self.bead_status.clone());
        map.insert("bead_priority".to_string(), self.bead_priority.to_string());
        map.insert(
            "bead_assignee".to_string(),
            self.bead_assignee
                .clone()
                .unwrap_or_else(|| "(unassigned)".to_string()),
        );
        map.insert("bead_labels".to_string(), self.bead_labels.clone());
        map.insert("workspace".to_string(), self.workspace.clone());
        map.insert("worker_id".to_string(), self.worker_id.clone());
        map.insert("created_at".to_string(), self.created_at.clone());
        map.insert("updated_at".to_string(), self.updated_at.clone());
        map.insert(
            "actor".to_string(),
            self.actor.clone().unwrap_or_else(|| "(none)".to_string()),
        );

        map
    }
}

impl Default for RenderContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Render a template by substituting placeholders with context values.
///
/// # Arguments
///
/// * `template` - Template string with `{placeholder}` syntax
/// * `context` - RenderContext containing values to substitute
///
/// # Returns
///
/// The rendered template string with all placeholders replaced.
///
/// # Example
///
/// ```no_run
/// use needle::template::{RenderContext, render};
///
/// let context = RenderContext {
///     bead_title: "Fix bug".to_string(),
///     workspace: "/path/to/workspace".to_string(),
///     ..Default::default()
/// };
///
/// let template = "Task: {bead_title} in {workspace}";
/// let rendered = render(template, &context);
/// assert_eq!(rendered, "Task: Fix bug in /path/to/workspace");
/// ```
pub fn render(template: &str, context: &RenderContext) -> String {
    let values = context.to_map();
    let mut result = template.to_string();

    for (placeholder, value) in &values {
        let pattern = format!("{{{}}}", placeholder);
        result = result.replace(&pattern, value);
    }

    result
}

/// Render a template with additional custom variables.
///
/// This function extends the standard RenderContext with extra variables
/// that may be specific to a particular strand or use case.
///
/// # Arguments
///
/// * `template` - Template string with `{placeholder}` syntax
/// * `context` - RenderContext containing values to substitute
/// * `extra_vars` - Additional (placeholder, value) pairs
///
/// # Example
///
/// ```no_run
/// use needle::template::{render_with_vars, RenderContext};
///
/// let context = RenderContext::default();
/// let template = "Status: {custom_status}";
/// let extra = vec![("custom_status".to_string(), "success".to_string())];
/// let rendered = render_with_vars(template, &context, &extra);
/// assert_eq!(rendered, "Status: success");
/// ```
pub fn render_with_vars(
    template: &str,
    context: &RenderContext,
    extra_vars: &[(String, String)],
) -> String {
    let mut result = render(template, context);

    for (placeholder, value) in extra_vars {
        let pattern = format!("{{{}}}", placeholder);
        result = result.replace(&pattern, value);
    }

    result
}

/// Extract all placeholder names from a template.
///
/// Returns a vector of unique placeholder names (without braces) found in
/// the template string.
///
/// # Example
///
/// ```
/// use needle::template::extract_placeholders;
///
/// let template = "Work on {bead_title} in {workspace}";
/// let placeholders = extract_placeholders(template);
/// assert_eq!(placeholders, vec!["bead_title", "workspace"]);
/// ```
pub fn extract_placeholders(template: &str) -> Vec<String> {
    let mut placeholders = Vec::new();
    let chars: Vec<char> = template.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == '{' {
            // Skip escaped braces: {{
            if i + 1 < len && chars[i + 1] == '{' {
                i += 2;
                continue;
            }

            // Find matching closing brace
            if let Some(end_offset) = chars[i + 1..].iter().position(|&c| c == '}') {
                let end = i + 1 + end_offset;
                let placeholder: String = chars[i + 1..end].iter().collect();

                // Only include if it looks like a valid placeholder (alphanumeric + underscore)
                if !placeholder.is_empty()
                    && placeholder
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !placeholders.contains(&placeholder)
                {
                    placeholders.push(placeholder);
                }

                i = end + 1;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    placeholders
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BeadId, BeadStatus};
    use chrono::Utc;
    use std::path::PathBuf;

    fn test_bead() -> Bead {
        Bead {
            id: BeadId::from("needle-test"),
            title: "Implement feature X".to_string(),
            body: Some("Add feature X to the system.".to_string()),
            priority: 1,
            status: BeadStatus::Open,
            assignee: Some("worker-01".to_string()),
            labels: vec!["feature".to_string(), "high-priority".to_string()],
            workspace: PathBuf::from("/home/coding/project"),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn render_basic_template() {
        let bead = test_bead();
        let context = RenderContext::from_bead(&bead, Path::new("/workspace"), "worker-01");

        let template = "Task: {bead_title}";
        let result = render(template, &context);

        assert_eq!(result, "Task: Implement feature X");
    }

    #[test]
    fn render_multiple_placeholders() {
        let bead = test_bead();
        let context = RenderContext::from_bead(&bead, Path::new("/workspace"), "worker-01");

        let template = "{bead_id}: {bead_title} in {workspace}";
        let result = render(template, &context);

        assert_eq!(result, "needle-test: Implement feature X in /workspace");
    }

    #[test]
    fn render_all_placeholders() {
        let bead = test_bead();
        let context =
            RenderContext::from_bead(&bead, Path::new("/home/coding/project"), "worker-01");

        let template = "{bead_id}|{bead_title}|{bead_body}|{bead_status}|{bead_priority}|{bead_assignee}|{bead_labels}|{workspace}|{worker_id}|{created_at}|{updated_at}|{actor}";
        let result = render(template, &context);

        assert!(result.contains("needle-test"));
        assert!(result.contains("Implement feature X"));
        assert!(result.contains("Add feature X"));
        assert!(result.contains("open"));
        assert!(result.contains("1"));
        assert!(result.contains("worker-01"));
        assert!(result.contains("feature, high-priority"));
        assert!(result.contains("/home/coding/project"));
    }

    #[test]
    fn render_missing_body_uses_fallback() {
        let mut bead = test_bead();
        bead.body = None;
        let context = RenderContext::from_bead(&bead, Path::new("/workspace"), "worker-01");

        let template = "Description: {bead_body}";
        let result = render(template, &context);

        assert_eq!(result, "Description: (no description)");
    }

    #[test]
    fn render_unassigned_uses_fallback() {
        let mut bead = test_bead();
        bead.assignee = None;
        let context = RenderContext::from_bead(&bead, Path::new("/workspace"), "worker-01");

        let template = "Assignee: {bead_assignee}";
        let result = render(template, &context);

        assert_eq!(result, "Assignee: (unassigned)");
    }

    #[test]
    fn render_empty_labels() {
        let mut bead = test_bead();
        bead.labels = vec![];
        let context = RenderContext::from_bead(&bead, Path::new("/workspace"), "worker-01");

        let template = "Labels: {bead_labels}";
        let result = render(template, &context);

        assert_eq!(result, "Labels: ");
    }

    #[test]
    fn render_with_no_placeholders() {
        let bead = test_bead();
        let context = RenderContext::from_bead(&bead, Path::new("/workspace"), "worker-01");

        let template = "This is just plain text";
        let result = render(template, &context);

        assert_eq!(result, "This is just plain text");
    }

    #[test]
    fn render_with_vars_custom_variables() {
        let context = RenderContext::default();

        let template = "Status: {custom_status}";
        let extra = vec![("custom_status".to_string(), "success".to_string())];
        let result = render_with_vars(template, &context, &extra);

        assert_eq!(result, "Status: success");
    }

    #[test]
    fn render_with_vars_mixed_variables() {
        let bead = test_bead();
        let context = RenderContext::from_bead(&bead, Path::new("/workspace"), "worker-01");

        let template = "{bead_title} has status {exit_code}";
        let extra = vec![("exit_code".to_string(), "0".to_string())];
        let result = render_with_vars(template, &context, &extra);

        assert_eq!(result, "Implement feature X has status 0");
    }

    #[test]
    fn extract_placeholders_basic() {
        let template = "{bead_id} and {bead_title}";
        let result = extract_placeholders(template);

        assert_eq!(result, vec!["bead_id", "bead_title"]);
    }

    #[test]
    fn extract_placeholders_deduplicates() {
        let template = "{bead_id} and {bead_id} again";
        let result = extract_placeholders(template);

        assert_eq!(result, vec!["bead_id"]);
    }

    #[test]
    fn extract_placeholders_skips_escaped_braces() {
        let template = r#"{{ "json": "value" }} and {bead_id}"#;
        let result = extract_placeholders(template);

        assert_eq!(result, vec!["bead_id"]);
    }

    #[test]
    fn extract_placeholders_empty_template() {
        let result = extract_placeholders("no placeholders here");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_placeholders_invalid_names() {
        let template = "{valid} and {invalid name} and {also-invalid}";
        let result = extract_placeholders(template);

        assert_eq!(result, vec!["valid"]);
    }

    #[test]
    fn render_context_from_bead() {
        let bead = test_bead();
        let context = RenderContext::from_bead(&bead, Path::new("/test/workspace"), "test-worker");

        assert_eq!(context.bead_id, "needle-test");
        assert_eq!(context.bead_title, "Implement feature X");
        assert_eq!(
            context.bead_body.as_ref().unwrap(),
            "Add feature X to the system."
        );
        assert_eq!(context.bead_status, "open");
        assert_eq!(context.bead_priority, 1);
        assert_eq!(context.bead_assignee.as_ref().unwrap(), "worker-01");
        assert_eq!(context.bead_labels, "feature, high-priority");
        assert_eq!(context.workspace, "/test/workspace");
        assert_eq!(context.worker_id, "test-worker");
        assert_eq!(context.actor.as_ref().unwrap(), "worker-01");
    }

    #[test]
    fn render_context_new_has_defaults() {
        let context = RenderContext::new();

        assert_eq!(context.bead_id, "");
        assert_eq!(context.bead_title, "");
        assert!(context.bead_body.is_none());
        assert_eq!(context.bead_status, "open");
        assert_eq!(context.bead_priority, 2);
        assert!(context.bead_assignee.is_none());
        assert_eq!(context.bead_labels, "");
        assert_eq!(context.workspace, "");
        assert_eq!(context.worker_id, "");
        assert!(context.actor.is_none());
    }

    #[test]
    fn render_cli_template() {
        let bead = test_bead();
        let context =
            RenderContext::from_bead(&bead, Path::new("/home/coding/project"), "worker-01");

        let template = "cd {workspace} && bead close {bead_id} --reason 'completed'";
        let result = render(template, &context);

        assert!(result.contains("cd /home/coding/project"));
        assert!(result.contains("bead close needle-test"));
        assert!(result.contains("--reason 'completed'"));
    }

    #[test]
    fn render_handles_timestamps() {
        let bead = test_bead();
        let context = RenderContext::from_bead(&bead, Path::new("/workspace"), "worker-01");

        let template = "Created at {created_at}, updated at {updated_at}";
        let result = render(template, &context);

        // Verify timestamps are in the expected format
        assert!(result.contains("Created at 20"));
        assert!(result.contains("UTC"));
    }

    #[test]
    fn render_handles_different_statuses() {
        for status in [
            BeadStatus::Open,
            BeadStatus::InProgress,
            BeadStatus::Done,
            BeadStatus::Blocked,
            BeadStatus::Deferred,
        ] {
            let expected_status = status.to_string();
            let mut bead = test_bead();
            bead.status = status.clone();
            let context = RenderContext::from_bead(&bead, Path::new("/workspace"), "worker-01");

            let template = "Status: {bead_status}";
            let result = render(template, &context);

            assert!(
                result.contains(&expected_status),
                "Status {} should render as {}",
                expected_status,
                expected_status
            );
        }
    }

    #[test]
    fn render_preserves_whitespace() {
        let bead = test_bead();
        let context = RenderContext::from_bead(&bead, Path::new("/workspace"), "worker-01");

        let template = "Line 1\n\n  Line 2\n\tLine 3";
        let result = render(template, &context);

        assert_eq!(result, template);
    }

    #[test]
    fn render_multiple_same_placeholder() {
        let bead = test_bead();
        let context = RenderContext::from_bead(&bead, Path::new("/workspace"), "worker-01");

        let template = "{bead_id} {bead_id} {bead_id}";
        let result = render(template, &context);

        assert_eq!(result, "needle-test needle-test needle-test");
    }

    #[test]
    fn render_handles_special_characters_in_values() {
        let mut bead = test_bead();
        bead.title = "Fix: bug with $100 cost & 5% increase".to_string();
        let context = RenderContext::from_bead(&bead, Path::new("/workspace"), "worker-01");

        let template = "Title: {bead_title}";
        let result = render(template, &context);

        assert_eq!(result, "Title: Fix: bug with $100 cost & 5% increase");
    }
}
