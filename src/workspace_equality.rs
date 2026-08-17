//! Comprehensive equality assertion for bead workspace state.
//!
//! This module provides tools for comparing two bead workspaces to verify that
//! a round-trip (flush + restore) preserves all public fields. Any field that
//! is not expected to be preserved must be explicitly excluded with documentation.
//!
//! # Example
//!
//! ```no_run
//! use needle::workspace_equality::{assert_workspace_eq, WorkspaceEqualityConfig};
//!
//! let config = WorkspaceEqualityConfig::default()
//!     .exclude_field("compaction_level", "Internal field, may increase during restore");
//!
//! assert_workspace_eq(&source_workspace, &restored_workspace, &config);
//! ```

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value as JsonValue;

/// Configuration for workspace equality comparison.
///
/// Allows specifying which fields to exclude from comparison, custom comparison
/// functions for complex fields, and tolerance for timestamp comparisons.
#[derive(Debug, Clone)]
pub struct WorkspaceEqualityConfig {
    /// Fields to exclude from comparison with documentation explaining why.
    /// Format: field_name -> reason_for_exclusion
    pub excluded_fields: HashMap<String, String>,

    /// Custom comparison functions for specific fields.
    /// Format: "bead_id.field_name" -> comparison function
    pub custom_comparators: HashMap<String, CustomComparator>,

    /// Maximum allowed difference between timestamps (in milliseconds).
    /// Set to None for exact matching.
    pub timestamp_tolerance_ms: Option<u64>,
}

impl Default for WorkspaceEqualityConfig {
    fn default() -> Self {
        let mut excluded_fields = HashMap::new();

        // Internal fields that are not part of the public surface
        excluded_fields.insert(
            "compaction_level".to_string(),
            "Internal SQLite VACUUM state, may increase during restore".to_string(),
        );
        excluded_fields.insert(
            "content_hash".to_string(),
            "Internal field, not part of public bead surface".to_string(),
        );
        excluded_fields.insert(
            "sender".to_string(),
            "Internal tracking field, not part of public surface".to_string(),
        );
        excluded_fields.insert(
            "ephemeral".to_string(),
            "Internal flag, not part of public surface".to_string(),
        );
        excluded_fields.insert(
            "pinned".to_string(),
            "Internal flag, not part of public surface".to_string(),
        );
        excluded_fields.insert(
            "is_template".to_string(),
            "Internal flag, not part of public surface".to_string(),
        );
        excluded_fields.insert(
            "manual_status".to_string(),
            "Internal override field, not part of public surface".to_string(),
        );
        excluded_fields.insert(
            "deleted_at".to_string(),
            "Soft-delete metadata, not part of active bead surface".to_string(),
        );
        excluded_fields.insert(
            "deleted_by".to_string(),
            "Soft-delete metadata, not part of active bead surface".to_string(),
        );
        excluded_fields.insert(
            "delete_reason".to_string(),
            "Soft-delete metadata, not part of active bead surface".to_string(),
        );

        Self {
            excluded_fields,
            custom_comparators: HashMap::new(),
            timestamp_tolerance_ms: None,
        }
    }
}

impl WorkspaceEqualityConfig {
    /// Create a new config with default exclusions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a field to the exclusion list with documentation.
    ///
    /// # Arguments
    ///
    /// * `field_name` - Name of the field to exclude (e.g., "updated_at")
    /// * `reason` - Why this field is excluded from comparison
    pub fn exclude_field(mut self, field_name: &str, reason: &str) -> Self {
        self.excluded_fields
            .insert(field_name.to_string(), reason.to_string());
        self
    }

    /// Add a custom comparator for a specific field path.
    ///
    /// Field paths use dot notation: "bead_id.field_name" or just "field_name" for all beads.
    ///
    /// # Arguments
    ///
    /// * `field_path` - Path to the field (e.g., "updated_at" or "bf-xyz.created_at")
    /// * `comparator` - Custom comparison function
    pub fn with_comparator(mut self, field_path: &str, comparator: CustomComparator) -> Self {
        self.custom_comparators
            .insert(field_path.to_string(), comparator);
        self
    }

    /// Set timestamp tolerance in milliseconds.
    ///
    /// # Arguments
    ///
    /// * `tolerance_ms` - Maximum allowed difference between timestamps
    pub fn with_timestamp_tolerance(mut self, tolerance_ms: u64) -> Self {
        self.timestamp_tolerance_ms = Some(tolerance_ms);
        self
    }
}

/// Custom comparison function for a specific field.
pub type CustomComparator = fn(&JsonValue, &JsonValue) -> Result<(), String>;

/// Result of comparing two workspaces.
#[derive(Debug)]
pub struct WorkspaceComparisonResult {
    /// Whether the workspaces are equal
    pub is_equal: bool,
    /// Differences found, if any
    pub differences: Vec<WorkspaceDifference>,
}

/// A difference found between two workspaces.
#[derive(Debug, Clone)]
pub struct WorkspaceDifference {
    /// Bead ID where the difference was found, or None for workspace-level differences
    pub bead_id: Option<String>,
    /// Field path that differs (e.g., "title", "dependencies[0].dependency_type")
    pub field_path: String,
    /// Expected value (from source workspace)
    pub expected: JsonValue,
    /// Actual value (from restored workspace)
    pub actual: JsonValue,
    /// Human-readable description of the difference
    pub description: String,
}

impl WorkspaceDifference {
    /// Create a new workspace difference.
    pub fn new(
        bead_id: Option<String>,
        field_path: String,
        expected: JsonValue,
        actual: JsonValue,
    ) -> Self {
        let description = format!(
            "Field '{}' differs: expected {}, got {}",
            field_path,
            format_value(&expected),
            format_value(&actual)
        );

        Self {
            bead_id,
            field_path,
            expected,
            actual,
            description,
        }
    }
}

/// Format a JSON value for display in error messages.
fn format_value(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Number(n) => n.to_string(),
        JsonValue::String(s) => {
            if s.len() > 50 {
                format!("\"{}...\"", &s[..50])
            } else {
                format!("\"{}\"", s)
            }
        }
        JsonValue::Array(arr) => format!("[... {} items ...]", arr.len()),
        JsonValue::Object(_) => "{...}".to_string(),
    }
}

/// Compare two bead workspaces for equality.
///
/// This function loads all beads from both workspaces and compares their complete
/// public surface, including:
/// - Core fields (id, title, description, priority, status, assignee, labels)
/// - Timestamps (created_at, updated_at, closed_at, deferred_at)
/// - Issue data (design, acceptance_criteria, issue_type, owner, estimate)
/// - Dependencies (both blocks and parent-child)
/// - Comments
/// - Events
/// - External references
/// - Close metadata (close_reason)
///
/// # Arguments
///
/// * `source_workspace` - Path to the source workspace
/// * `restored_workspace` - Path to the restored/workspace to compare against
/// * `config` - Comparison configuration
///
/// # Panics
///
/// Panics if the workspaces are not equal, with a detailed diff showing all differences.
///
/// # Example
///
/// ```no_run
/// use needle::workspace_equality::{assert_workspace_eq, WorkspaceEqualityConfig};
///
/// let config = WorkspaceEqualityConfig::default();
/// assert_workspace_eq(
///     &PathBuf::from("/tmp/source"),
///     &PathBuf::from("/tmp/restore"),
///     &config
/// );
/// ```
pub fn assert_workspace_eq(
    source_workspace: &Path,
    restored_workspace: &Path,
    config: &WorkspaceEqualityConfig,
) {
    let result = compare_workspaces(source_workspace, restored_workspace, config);

    if !result.is_equal {
        let diff_report = format_diff_report(&result.differences);
        panic!(
            "Workspace comparison failed:\n\
             Source: {}\n\
             Restored: {}\n\
             Differences found:\n\
             {}",
            source_workspace.display(),
            restored_workspace.display(),
            diff_report
        );
    }
}

/// Compare two workspaces and return the result without panicking.
pub fn compare_workspaces(
    source_workspace: &Path,
    restored_workspace: &Path,
    config: &WorkspaceEqualityConfig,
) -> WorkspaceComparisonResult {
    let mut differences = Vec::new();

    // Load beads from both workspaces
    let source_beads = load_all_beads(source_workspace);
    let restored_beads = load_all_beads(restored_workspace);

    // Check bead count
    if source_beads.len() != restored_beads.len() {
        differences.push(WorkspaceDifference {
            bead_id: None,
            field_path: "bead_count".to_string(),
            expected: JsonValue::Number(source_beads.len().into()),
            actual: JsonValue::Number(restored_beads.len().into()),
            description: format!(
                "Bead count mismatch: source has {}, restored has {}",
                source_beads.len(),
                restored_beads.len()
            ),
        });
        return WorkspaceComparisonResult {
            is_equal: false,
            differences,
        };
    }

    // Build ID map for restored beads
    let restored_map: HashMap<_, _> = restored_beads
        .into_iter()
        .filter_map(|b| {
            b.get("id")
                .and_then(|id| id.as_str())
                .map(|id| (id.to_string(), b.clone()))
        })
        .collect();

    // Compare each bead
    for source_bead in &source_beads {
        let bead_id = source_bead
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Clone and sort for stable comparison
        let mut source_clone = source_bead.clone();
        sort_bead_collections(&mut source_clone);

        if let Some(restored_bead) = restored_map.get(&bead_id) {
            let mut restored_clone = restored_bead.clone();
            sort_bead_collections(&mut restored_clone);

            compare_beads(
                &bead_id,
                &source_clone,
                &restored_clone,
                config,
                &mut differences,
            );
        } else {
            differences.push(WorkspaceDifference {
                bead_id: Some(bead_id.clone()),
                field_path: "bead_presence".to_string(),
                expected: JsonValue::String(format!("bead {} exists", bead_id)),
                actual: JsonValue::Null,
                description: format!("Bead {} exists in source but not in restored", bead_id),
            });
        }
    }

    // Check for extra beads in restored
    for bead_id in restored_map.keys() {
        let exists_in_source = source_beads
            .iter()
            .any(|b| b.get("id").and_then(|v| v.as_str()) == Some(bead_id));

        if !exists_in_source {
            differences.push(WorkspaceDifference {
                bead_id: Some(bead_id.clone()),
                field_path: "bead_presence".to_string(),
                expected: JsonValue::Null,
                actual: JsonValue::String(format!("bead {} exists", bead_id)),
                description: format!("Bead {} exists in restored but not in source", bead_id),
            });
        }
    }

    WorkspaceComparisonResult {
        is_equal: differences.is_empty(),
        differences,
    }
}

/// Load all beads from a workspace as JSON values.
fn load_all_beads(workspace: &Path) -> Vec<JsonValue> {
    use std::process::Command;

    let output = Command::new("bead")
        .args(["list", "--json", "--limit", "999999"])
        .current_dir(workspace)
        .output()
        .expect("bead list failed");

    if !output.status.success() {
        panic!(
            "bead list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout).expect("bead output was not UTF-8");
    if stdout.trim() == "[]" {
        return Vec::new();
    }

    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("invalid bead JSON from bead list"))
        .collect()
}

/// Sort collections within a bead for stable comparison.
fn sort_bead_collections(bead: &mut JsonValue) {
    if let Some(obj) = bead.as_object_mut() {
        if let Some(deps) = obj.get_mut("dependencies").and_then(|v| v.as_array_mut()) {
            deps.sort_by(|a, b| {
                a.get("depends_on_id")
                    .and_then(|v| v.as_str())
                    .cmp(&b.get("depends_on_id").and_then(|v| v.as_str()))
            });
        }

        if let Some(comments) = obj.get_mut("comments").and_then(|v| v.as_array_mut()) {
            comments.sort_by(|a, b| {
                a.get("id")
                    .and_then(|v| v.as_i64())
                    .cmp(&b.get("id").and_then(|v| v.as_i64()))
            });
        }

        if let Some(events) = obj.get_mut("events").and_then(|v| v.as_array_mut()) {
            events.sort_by(|a, b| {
                a.get("id")
                    .and_then(|v| v.as_i64())
                    .cmp(&b.get("id").and_then(|v| v.as_i64()))
            });
        }

        if let Some(labels) = obj.get_mut("labels").and_then(|v| v.as_array_mut()) {
            labels.sort_by(|a, b| {
                // Sort labels as strings if possible, fallback to JSON string representation
                match (a.as_str(), b.as_str()) {
                    (Some(a_str), Some(b_str)) => a_str.cmp(b_str),
                    _ => a.to_string().cmp(&b.to_string()),
                }
            });
        }
    }
}

/// Compare two beads field by field.
fn compare_beads(
    bead_id: &str,
    source: &JsonValue,
    restored: &JsonValue,
    config: &WorkspaceEqualityConfig,
    differences: &mut Vec<WorkspaceDifference>,
) {
    let source_obj = source.as_object().expect("source bead is not an object");
    let restored_obj = restored
        .as_object()
        .expect("restored bead is not an object");

    // Get all field names from both beads
    let all_fields: std::collections::HashSet<_> =
        source_obj.keys().chain(restored_obj.keys()).collect();

    for field in all_fields {
        // Skip excluded fields
        if config.excluded_fields.contains_key(field) {
            continue;
        }

        let source_val = source_obj.get(field);
        let restored_val = restored_obj.get(field);

        // Check for custom comparator
        let field_path = format!("{}.{}", bead_id, field);

        if let Some(comparator) = config
            .custom_comparators
            .get(&field_path)
            .or_else(|| config.custom_comparators.get(field))
        {
            if let (Some(s), Some(r)) = (source_val, restored_val) {
                if let Err(msg) = comparator(s, r) {
                    differences.push(WorkspaceDifference {
                        bead_id: Some(bead_id.to_string()),
                        field_path: field.clone(),
                        expected: s.clone(),
                        actual: r.clone(),
                        description: msg,
                    });
                }
            }
            continue;
        }

        // Default comparison logic
        match (source_val, restored_val) {
            (None, Some(r)) => {
                differences.push(WorkspaceDifference::new(
                    Some(bead_id.to_string()),
                    field.clone(),
                    JsonValue::Null,
                    r.clone(),
                ));
            }
            (Some(s), None) => {
                differences.push(WorkspaceDifference::new(
                    Some(bead_id.to_string()),
                    field.clone(),
                    s.clone(),
                    JsonValue::Null,
                ));
            }
            (Some(s), Some(r)) => {
                if !values_equal(s, r, field, config) {
                    differences.push(WorkspaceDifference::new(
                        Some(bead_id.to_string()),
                        field.clone(),
                        s.clone(),
                        r.clone(),
                    ));
                }
            }
            (None, None) => {} // Both null, equal
        }
    }
}

/// Compare two JSON values with special handling for timestamps and arrays.
fn values_equal(
    a: &JsonValue,
    b: &JsonValue,
    field: &str,
    config: &WorkspaceEqualityConfig,
) -> bool {
    // Check for timestamp fields
    if is_timestamp_field(field) {
        return timestamps_equal(a, b, config.timestamp_tolerance_ms);
    }

    match (a, b) {
        (JsonValue::Null, JsonValue::Null) => true,
        (JsonValue::Bool(a_val), JsonValue::Bool(b_val)) => a_val == b_val,
        (JsonValue::Number(a_val), JsonValue::Number(b_val)) => {
            // Compare numbers with tolerance if configured
            if let Some(tol) = config.timestamp_tolerance_ms {
                if let (Some(a_i), Some(b_i)) = (a_val.as_i64(), b_val.as_i64()) {
                    let diff = (a_i - b_i).abs();
                    return diff <= tol as i64;
                }
            }
            a_val == b_val
        }
        (JsonValue::String(a_val), JsonValue::String(b_val)) => a_val == b_val,
        (JsonValue::Array(a_arr), JsonValue::Array(b_arr)) => {
            if a_arr.len() != b_arr.len() {
                return false;
            }
            a_arr
                .iter()
                .zip(b_arr.iter())
                .all(|(a_item, b_item)| values_equal(a_item, b_item, field, config))
        }
        (JsonValue::Object(a_obj), JsonValue::Object(b_obj)) => {
            if a_obj.len() != b_obj.len() {
                return false;
            }
            a_obj
                .iter()
                .zip(b_obj.iter())
                .all(|((k1, v1), (k2, v2))| k1 == k2 && values_equal(v1, v2, field, config))
        }
        _ => false, // Different types
    }
}

/// Check if a field is a timestamp field.
fn is_timestamp_field(field: &str) -> bool {
    matches!(
        field,
        "created_at" | "updated_at" | "closed_at" | "deferred_at" | "reopened_at"
    )
}

/// Compare two timestamp values with optional tolerance.
fn timestamps_equal(a: &JsonValue, b: &JsonValue, tolerance_ms: Option<u64>) -> bool {
    let (a_str, b_str) = match (a.as_str(), b.as_str()) {
        (Some(a_s), Some(b_s)) => (a_s, b_s),
        _ => return a == b, // If not strings, use default equality
    };

    // Parse timestamps
    let a_dt = chrono::DateTime::parse_from_rfc3339(a_str);
    let b_dt = chrono::DateTime::parse_from_rfc3339(b_str);

    match (a_dt, b_dt) {
        (Ok(a_time), Ok(b_time)) => {
            if let Some(tol) = tolerance_ms {
                let diff = (a_time - b_time).num_milliseconds().abs();
                diff <= tol as i64
            } else {
                a_time == b_time
            }
        }
        _ => a == b, // If parsing fails, use default equality
    }
}

/// Format a human-readable diff report from differences.
fn format_diff_report(differences: &[WorkspaceDifference]) -> String {
    if differences.is_empty() {
        return "No differences found".to_string();
    }

    let mut report = String::new();

    // Group by bead ID
    let mut grouped: std::collections::HashMap<Option<&str>, Vec<&WorkspaceDifference>> =
        std::collections::HashMap::new();

    for diff in differences {
        grouped
            .entry(diff.bead_id.as_deref())
            .or_default()
            .push(diff);
    }

    // Sort groups: workspace-level first, then by bead ID
    let mut sorted_groups: Vec<_> = grouped.into_iter().collect();
    sorted_groups.sort_by(|(id1, _), (id2, _)| match (id1, id2) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(a), Some(b)) => a.cmp(b),
    });

    for (bead_id, diffs) in sorted_groups {
        if let Some(id) = bead_id {
            report.push_str(&format!("\nBead {}:\n", id));
        } else {
            report.push_str("\nWorkspace-level:\n");
        }

        for diff in diffs {
            report.push_str(&format!("  - {}\n", diff.description));
            report.push_str(&format!(
                "    Expected: {}\n",
                serde_json::to_string(&diff.expected).unwrap_or_else(|_| "<?>".to_string())
            ));
            report.push_str(&format!(
                "    Got:      {}\n",
                serde_json::to_string(&diff.actual).unwrap_or_else(|_| "<?>".to_string())
            ));
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_config_default_exclusions() {
        let config = WorkspaceEqualityConfig::default();
        assert!(config.excluded_fields.contains_key("compaction_level"));
        assert!(config.excluded_fields.contains_key("content_hash"));
    }

    #[test]
    fn test_config_exclude_field() {
        let config = WorkspaceEqualityConfig::default()
            .exclude_field("updated_at", "Changes during restore");

        assert_eq!(
            config.excluded_fields.get("updated_at"),
            Some(&"Changes during restore".to_string())
        );
    }

    #[test]
    fn test_config_timestamp_tolerance() {
        let config = WorkspaceEqualityConfig::default().with_timestamp_tolerance(1000);
        assert_eq!(config.timestamp_tolerance_ms, Some(1000));
    }

    #[test]
    fn test_format_value() {
        assert_eq!(format_value(&json!("short")), "\"short\"");
        assert_eq!(format_value(&json!(42)), "42");
        assert_eq!(format_value(&json!(true)), "true");
        assert_eq!(format_value(&JsonValue::Null), "null");
        assert_eq!(format_value(&json!([1, 2, 3])), "[... 3 items ...]");
        assert_eq!(
            format_value(&json!(
                "This is a very long string that exceeds fifty characters in length"
            )),
            "\"This is a very long string that exceeds fifty char...\""
        );
    }

    #[test]
    fn test_is_timestamp_field() {
        assert!(is_timestamp_field("created_at"));
        assert!(is_timestamp_field("updated_at"));
        assert!(is_timestamp_field("closed_at"));
        assert!(!is_timestamp_field("title"));
        assert!(!is_timestamp_field("priority"));
    }

    #[test]
    fn test_timestamps_equal() {
        let t1 = json!("2026-08-13T12:00:00Z");
        let t2 = json!("2026-08-13T12:00:00Z");
        assert!(timestamps_equal(&t1, &t2, None));

        let t3 = json!("2026-08-13T12:00:01Z");
        assert!(!timestamps_equal(&t1, &t3, None));
        assert!(timestamps_equal(&t1, &t3, Some(2000)));
    }

    #[test]
    fn test_sort_bead_collections() {
        let mut bead = json!({
            "id": "test-1",
            "labels": ["zebra", "apple", "banana"],
            "comments": [
                {"id": 3, "text": "third"},
                {"id": 1, "text": "first"}
            ],
            "dependencies": [
                {"depends_on_id": "zzz"},
                {"depends_on_id": "aaa"}
            ]
        });

        sort_bead_collections(&mut bead);

        assert_eq!(
            bead["labels"].as_array(),
            Some(
                &vec!["apple", "banana", "zebra"]
                    .into_iter()
                    .map(JsonValue::from)
                    .collect::<Vec<_>>()
            )
        );

        assert_eq!(bead["comments"][0]["id"], 1);
        assert_eq!(bead["comments"][1]["id"], 3);

        assert_eq!(bead["dependencies"][0]["depends_on_id"], "aaa");
        assert_eq!(bead["dependencies"][1]["depends_on_id"], "zzz");
    }
}
