//! Learning and retrospective extraction from bead close bodies.
//!
//! The `Retrospective` type parses structured learning blocks from bead close
//! messages. Agents write retrospectives when closing beads, and the consolidator
//! (reflect strand) extracts patterns for workspace learning.

use anyhow::Result;
use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────────────
// Retrospective
// ──────────────────────────────────────────────────────────────────────────────

/// A structured retrospective written by an agent when closing a bead.
///
/// Retrospectives capture learning from each completed task:
/// - What worked: approaches that succeeded
/// - What didn't: approaches that failed and why
/// - Surprise: anything unexpected about the codebase or tooling
/// - Reusable pattern: if this task type recurs, do X
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Retrospective {
    /// Approaches that succeeded in this task.
    pub what_worked: Option<String>,
    /// Approaches that failed, with explanations.
    pub what_didnt: Option<String>,
    /// Unexpected discoveries about the codebase or tooling.
    pub surprise: Option<String>,
    /// Reusable patterns for similar future tasks.
    pub reusable_pattern: Option<String>,
}

impl Retrospective {
    /// Parse a retrospective from a bead close body.
    ///
    /// Looks for a "## Retrospective" header and extracts the four fields
    /// using markdown list format (`- **Field:** value`).
    ///
    /// Returns `Ok(None)` if no retrospective block is found.
    pub fn parse_from_close_body(body: &str) -> Result<Option<Retrospective>> {
        // Find the retrospective section
        let retro_start = body.find("## Retrospective");
        let retro_content = match retro_start {
            Some(idx) => {
                // Content starts after the header line
                let after_header = idx + "## Retrospective".len();
                // Skip to next line
                let content_start = body[after_header..]
                    .find('\n')
                    .map_or(after_header, |n| after_header + n + 1);
                // Content extends to the next "##" header or end of string
                let next_header = body[content_start..].find("\n##");
                match next_header {
                    Some(n) => &body[content_start..content_start + n],
                    None => &body[content_start..],
                }
            }
            None => return Ok(None),
        };

        // Parse each field using the `- **Field:** value` format
        let what_worked = Self::extract_field(retro_content, "What worked");
        let what_didnt = Self::extract_field(retro_content, "What didn't");
        let surprise = Self::extract_field(retro_content, "Surprise");
        let reusable_pattern = Self::extract_field(retro_content, "Reusable pattern");

        // If all fields are None, treat as no retrospective found
        if what_worked.is_none()
            && what_didnt.is_none()
            && surprise.is_none()
            && reusable_pattern.is_none()
        {
            return Ok(None);
        }

        Ok(Some(Retrospective {
            what_worked,
            what_didnt,
            surprise,
            reusable_pattern,
        }))
    }

    /// Extract a single field value from the retrospective content.
    ///
    /// Looks for `- **FieldName:** value` and returns the trimmed value.
    fn extract_field(content: &str, field_name: &str) -> Option<String> {
        let marker = &format!("- **{}:**", field_name);
        let marker_idx = content.find(marker)?;
        let value_start = marker_idx + marker.len();

        // Find the end of the value (next list item or end of content)
        let value_end = content[value_start..]
            .find("\n-")
            .or_else(|| content[value_start..].find('\n'))
            .unwrap_or(content[value_start..].len());

        let value = content[value_start..value_start + value_end].trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    }

    /// Returns true if this retrospective has at least one non-empty field.
    pub fn is_meaningful(&self) -> bool {
        self.what_worked.is_some()
            || self.what_didnt.is_some()
            || self.surprise.is_some()
            || self.reusable_pattern.is_some()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn full_retrospective_body() -> String {
        r#"Implemented the feature successfully.

## Retrospective
- **What worked:** Used the existing pattern from similar modules
- **What didn't:** Initial attempt to use mutex failed due to async context
- **Surprise:** The BeadStatus enum has more variants than documented
- **Reusable pattern:** For new strands, copy the pluck.rs template and modify

Closed successfully."#
            .to_string()
    }

    #[test]
    fn parse_full_retrospective() {
        let body = full_retrospective_body();
        let result = Retrospective::parse_from_close_body(&body).unwrap();

        assert!(result.is_some());
        let retro = result.unwrap();
        assert_eq!(
            retro.what_worked.as_deref(),
            Some("Used the existing pattern from similar modules")
        );
        assert_eq!(
            retro.what_didnt.as_deref(),
            Some("Initial attempt to use mutex failed due to async context")
        );
        assert_eq!(
            retro.surprise.as_deref(),
            Some("The BeadStatus enum has more variants than documented")
        );
        assert_eq!(
            retro.reusable_pattern.as_deref(),
            Some("For new strands, copy the pluck.rs template and modify")
        );
    }

    #[test]
    fn parse_partial_retrospective() {
        let body = r#"Fixed the bug.

## Retrospective
- **What worked:** Adding debug logging revealed the issue quickly
- **Surprise:** The error was in a dependency, not our code

Done."#;

        let result = Retrospective::parse_from_close_body(body).unwrap();
        assert!(result.is_some());

        let retro = result.unwrap();
        assert_eq!(
            retro.what_worked.as_deref(),
            Some("Adding debug logging revealed the issue quickly")
        );
        assert!(retro.what_didnt.is_none());
        assert_eq!(
            retro.surprise.as_deref(),
            Some("The error was in a dependency, not our code")
        );
        assert!(retro.reusable_pattern.is_none());
    }

    #[test]
    fn parse_body_without_retrospective() {
        let body = "Completed the task. All tests pass.";
        let result = Retrospective::parse_from_close_body(body).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_retrospective_with_empty_fields() {
        let body = r#"Done.

## Retrospective
- **What worked:**
- **What didn't:** Nothing went wrong
- **Surprise:**
- **Reusable pattern:** N/A

Finished."#;

        let result = Retrospective::parse_from_close_body(body).unwrap();
        assert!(result.is_some());

        let retro = result.unwrap();
        assert!(retro.what_worked.is_none()); // Empty field becomes None
        assert_eq!(retro.what_didnt.as_deref(), Some("Nothing went wrong"));
        assert!(retro.surprise.is_none());
        assert_eq!(retro.reusable_pattern.as_deref(), Some("N/A"));
    }

    #[test]
    fn parse_retrospective_case_sensitive() {
        // Field names are case-sensitive - must match exact format
        let body = r#"Done.

## Retrospective
- **WHAT WORKED:** Uppercase field name
- **What worked:** Correct case

Closed."#;

        let result = Retrospective::parse_from_close_body(body).unwrap();
        assert!(result.is_some());

        let retro = result.unwrap();
        // Only the correctly-cased field should be parsed
        assert_eq!(retro.what_worked.as_deref(), Some("Correct case"));
    }

    #[test]
    fn parse_retrospective_all_empty_returns_none() {
        // If a retrospective header exists but all fields are empty, return None
        let body = r#"Done.

## Retrospective
- **What worked:**
- **What didn't:**
- **Surprise:**
- **Reusable pattern:**

Closed."#;

        let result = Retrospective::parse_from_close_body(body).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_retrospective_multiline_values() {
        // Multi-line values are captured until next field or end
        let body = r#"Done.

## Retrospective
- **What worked:** First approach worked well.
  Had to adjust slightly, but overall good.
- **What didn't:** Second approach failed due to
  async runtime incompatibility
- **Surprise:** Single line
- **Reusable pattern:** One line pattern

Closed."#;

        let result = Retrospective::parse_from_close_body(body).unwrap();
        assert!(result.is_some());

        let retro = result.unwrap();
        assert!(retro
            .what_worked
            .as_deref()
            .unwrap()
            .contains("First approach"));
        assert!(retro
            .what_didnt
            .as_deref()
            .unwrap()
            .contains("async runtime"));
    }

    #[test]
    fn meaningful_returns_true_with_content() {
        let retro = Retrospective {
            what_worked: Some("Good".to_string()),
            what_didnt: None,
            surprise: None,
            reusable_pattern: None,
        };
        assert!(retro.is_meaningful());
    }

    #[test]
    fn meaningful_returns_false_with_all_none() {
        let retro = Retrospective {
            what_worked: None,
            what_didnt: None,
            surprise: None,
            reusable_pattern: None,
        };
        assert!(!retro.is_meaningful());
    }

    #[test]
    fn serialize_retrospective_to_json() {
        let retro = Retrospective {
            what_worked: Some("Worked well".to_string()),
            what_didnt: Some("Failed approach".to_string()),
            surprise: None,
            reusable_pattern: Some("Use this pattern".to_string()),
        };

        let json = serde_json::to_string(&retro).unwrap();
        let parsed: Retrospective = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.what_worked, retro.what_worked);
        assert_eq!(parsed.what_didnt, retro.what_didnt);
        assert_eq!(parsed.surprise, retro.surprise);
        assert_eq!(parsed.reusable_pattern, retro.reusable_pattern);
    }
}
