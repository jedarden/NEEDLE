//! Learning and retrospective extraction from bead close bodies.
//!
//! The `Retrospective` type parses structured learning blocks from bead close
//! messages. Agents write retrospectives when closing beads, and the consolidator
//! (reflect strand) extracts patterns for workspace learning.
//!
//! ## Workspace Learnings
//!
//! Each workspace can maintain a `.beads/learnings.md` file that captures
//! learnings from completed beads. These are automatically injected into prompts
//! to inform future work.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────────────
// Learning Entry Types
// ──────────────────────────────────────────────────────────────────────────────

/// Confidence level for a learning entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    /// Parse from string (case-insensitive).
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "high" => Some(Confidence::High),
            "medium" => Some(Confidence::Medium),
            "low" => Some(Confidence::Low),
            _ => None,
        }
    }

    /// Convert to display string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }
}

/// Type of bead that produced this learning.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BeadType {
    BugFix,
    Feature,
    Refactor,
    Test,
    Documentation,
    Other,
}

impl BeadType {
    /// Parse from string (case-insensitive, supports various formats).
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().replace('_', "-").as_str() {
            "bug-fix" | "bugfix" | "bug" => Some(BeadType::BugFix),
            "feature" => Some(BeadType::Feature),
            "refactor" => Some(BeadType::Refactor),
            "test" | "testing" => Some(BeadType::Test),
            "documentation" | "docs" | "doc" => Some(BeadType::Documentation),
            "other" => Some(BeadType::Other),
            _ => None,
        }
    }

    /// Convert to display string.
    pub fn as_str(&self) -> &'static str {
        match self {
            BeadType::BugFix => "bug-fix",
            BeadType::Feature => "feature",
            BeadType::Refactor => "refactor",
            BeadType::Test => "test",
            BeadType::Documentation => "documentation",
            BeadType::Other => "other",
        }
    }
}

/// A single learning entry from a completed bead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningEntry {
    /// Date the learning was captured (YYYY-MM-DD).
    pub date: String,
    /// Bead ID that produced this learning.
    pub bead_id: String,
    /// Worker that handled the bead.
    pub worker: String,
    /// Type of bead work.
    pub bead_type: BeadType,
    /// What was discovered or learned.
    pub observation: String,
    /// Confidence level in this learning.
    pub confidence: Confidence,
    /// Source description (e.g., "retrospective from bead nd-a3f8").
    pub source: String,
    /// Reinforcement count (number of times this learning was referenced).
    #[serde(default)]
    pub reinforcement_count: u32,
    /// Last time this entry was reinforced.
    #[serde(default)]
    pub last_reinforced: Option<DateTime<Utc>>,
}

impl LearningEntry {
    /// Create a new learning entry.
    pub fn new(
        bead_id: String,
        worker: String,
        bead_type: BeadType,
        observation: String,
        confidence: Confidence,
        source: String,
    ) -> Self {
        let date = Utc::now().format("%Y-%m-%d").to_string();
        LearningEntry {
            date,
            bead_id,
            worker,
            bead_type,
            observation,
            confidence,
            source,
            reinforcement_count: 0,
            last_reinforced: None,
        }
    }

    /// Returns true if this entry is stale (older than 90 days without reinforcement).
    pub fn is_stale(&self) -> bool {
        use chrono::Duration;

        let days_since_creation =
            match chrono::DateTime::parse_from_rfc3339(&format!("{}T00:00:00Z", self.date)) {
                Ok(dt) => Utc::now() - dt.with_timezone(&Utc),
                Err(_) => return true, // Invalid date = stale
            };

        // Check last reinforced date if available
        if let Some(last_reinforced) = self.last_reinforced {
            let days_since_reinforcement = Utc::now() - last_reinforced;
            days_since_reinforcement > Duration::days(90)
        } else {
            // No reinforcement = use creation date
            days_since_creation > Duration::days(90)
        }
    }

    /// Increment the reinforcement counter and update last reinforced time.
    pub fn reinforce(&mut self) {
        self.reinforcement_count += 1;
        self.last_reinforced = Some(Utc::now());
    }

    /// Format as markdown for the learnings.md file.
    pub fn to_markdown(&self) -> String {
        format!(
            "### {} | bead: {} | worker: {} | type: {} | reinforced: {}\
             \n- **Observation:** {}\
             \n- **Confidence:** {}\
             \n- **Source:** {}\
             \n",
            self.date,
            self.bead_id,
            self.worker,
            self.bead_type.as_str(),
            self.reinforcement_count,
            self.observation,
            self.confidence.as_str(),
            self.source
        )
    }

    /// Parse a learning entry from markdown format.
    pub fn from_markdown(markdown: &str) -> Result<Self> {
        // Expected format:
        // ### 2026-04-04 | bead: nd-a3f8 | worker: alpha | type: bug-fix | reinforced: 0
        // - **Observation:** [what was discovered]
        // - **Confidence:** high/medium/low
        // - **Source:** retrospective from bead nd-a3f8

        let lines: Vec<&str> = markdown.lines().collect();
        if lines.len() < 4 {
            anyhow::bail!("Invalid learning entry: too few lines");
        }

        // Parse header line
        let header = lines[0]
            .strip_prefix("### ")
            .ok_or_else(|| anyhow::anyhow!("Invalid learning entry: missing header marker"))?;

        let mut date = None;
        let mut bead_id = None;
        let mut worker = None;
        let mut bead_type = None;
        let mut reinforcement_count = 0;

        for part in header.split(" | ") {
            let part = part.trim();
            if let Some(d) = part.strip_prefix("date: ") {
                date = Some(d.to_string());
            } else if let Some(d) = part.strip_prefix("bead: ") {
                bead_id = Some(d.to_string());
            } else if let Some(w) = part.strip_prefix("worker: ") {
                worker = Some(w.to_string());
            } else if let Some(t) = part.strip_prefix("type: ") {
                bead_type = BeadType::from_str(t);
            } else if let Some(r) = part.strip_prefix("reinforced: ") {
                reinforcement_count = r.parse().unwrap_or(0);
            }
        }

        // Legacy format: date is first unlabelled part
        if date.is_none() {
            let first_part = header.split(" | ").next();
            if let Some(d) = first_part {
                if !d.contains(':') {
                    date = Some(d.to_string());
                }
            }
        }

        let date = date.ok_or_else(|| anyhow::anyhow!("Missing date"))?;
        let bead_id = bead_id.ok_or_else(|| anyhow::anyhow!("Missing bead_id"))?;
        let worker = worker.ok_or_else(|| anyhow::anyhow!("Missing worker"))?;
        let bead_type = bead_type.ok_or_else(|| anyhow::anyhow!("Missing bead_type"))?;

        // Parse fields
        let mut observation = None;
        let mut confidence = None;
        let mut source = None;

        for line in &lines[1..] {
            if let Some(obs) = line.strip_prefix("- **Observation:**") {
                observation = Some(obs.trim().to_string());
            } else if let Some(conf) = line.strip_prefix("- **Confidence:**") {
                confidence = Confidence::from_str(conf.trim());
            } else if let Some(src) = line.strip_prefix("- **Source:**") {
                source = Some(src.trim().to_string());
            }
        }

        let observation = observation.ok_or_else(|| anyhow::anyhow!("Missing observation"))?;
        let confidence = confidence.ok_or_else(|| anyhow::anyhow!("Missing confidence"))?;
        let source = source.ok_or_else(|| anyhow::anyhow!("Missing source"))?;

        Ok(LearningEntry {
            date,
            bead_id,
            worker,
            bead_type,
            observation,
            confidence,
            source,
            reinforcement_count,
            last_reinforced: None,
        })
    }
}

/// Manages the `.beads/learnings.md` file for a workspace.
#[derive(Debug, Clone)]
pub struct LearningsFile {
    /// Path to the learnings file.
    path: PathBuf,
    /// Loaded entries.
    entries: Vec<LearningEntry>,
}

impl LearningsFile {
    /// Load or create the learnings file for a workspace.
    pub fn load(workspace: &Path) -> Result<Self> {
        let path = workspace.join(".beads").join("learnings.md");

        let entries = if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read learnings file: {}", path.display()))?;
            Self::parse_entries(&content)?
        } else {
            Vec::new()
        };

        Ok(LearningsFile { path, entries })
    }

    /// Parse entries from markdown content.
    fn parse_entries(content: &str) -> Result<Vec<LearningEntry>> {
        let mut entries = Vec::new();

        // Split by "### " to find each entry
        for chunk in content.split("### ") {
            let chunk = chunk.trim();
            if chunk.is_empty() {
                continue;
            }

            // Re-add the "### " for proper parsing
            let full_entry = format!("### {}", chunk);

            match LearningEntry::from_markdown(&full_entry) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    tracing::warn!("failed to parse learning entry: {}, skipping", e);
                }
            }
        }

        Ok(entries)
    }

    /// Get all entries.
    pub fn entries(&self) -> &[LearningEntry] {
        &self.entries
    }

    /// Add a new entry to the learnings file.
    pub fn add_entry(&mut self, entry: LearningEntry) -> Result<()> {
        self.entries.push(entry);
        self.write()?;
        Ok(())
    }

    /// Reinforce an existing entry by bead ID.
    pub fn reinforce_entry(&mut self, bead_id: &str) -> Result<bool> {
        let found = self.entries.iter_mut().any(|e| {
            if e.bead_id == bead_id {
                e.reinforce();
                true
            } else {
                false
            }
        });

        if found {
            self.write()?;
        }

        Ok(found)
    }

    /// Prune stale entries (older than 90 days without reinforcement).
    pub fn prune_stale(&mut self) -> Result<usize> {
        let original_len = self.entries.len();
        self.entries.retain(|e| !e.is_stale());

        if self.entries.len() != original_len {
            self.write()?;
        }

        Ok(original_len - self.entries.len())
    }

    /// Consolidate entries when exceeding max_count.
    ///
    /// Keeps high-confidence entries, entries with high reinforcement,
    /// and recent entries. Removes low-value entries.
    pub fn consolidate(&mut self, max_count: usize) -> Result<usize> {
        if self.entries.len() <= max_count {
            return Ok(0);
        }

        // Score entries: reinforcement * 10 + confidence_score
        // Confidence: high=3, medium=2, low=1
        let mut scored: Vec<_> = self
            .entries
            .iter()
            .map(|e| {
                let confidence_score = match e.confidence {
                    Confidence::High => 3,
                    Confidence::Medium => 2,
                    Confidence::Low => 1,
                };
                let score = (e.reinforcement_count as usize * 10) + confidence_score;
                (score, e)
            })
            .collect();

        // Sort by score descending
        scored.sort_by(|a, b| b.0.cmp(&a.0));

        // Keep top max_count entries
        let kept: Vec<LearningEntry> = scored
            .into_iter()
            .take(max_count)
            .map(|(_, e)| e.clone())
            .collect();

        let removed = self.entries.len() - kept.len();
        self.entries = kept;
        self.write()?;

        Ok(removed)
    }

    /// Write entries to the file.
    fn write(&self) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory: {}", parent.display()))?;
        }

        let mut content = String::from("# Workspace Learnings\n\n");
        content.push_str("This file is automatically managed by NEEDLE. Learnings from completed beads are captured here.\n\n");

        for entry in &self.entries {
            content.push_str(&entry.to_markdown());
            content.push('\n');
        }

        std::fs::write(&self.path, content)
            .with_context(|| format!("failed to write learnings file: {}", self.path.display()))?;

        Ok(())
    }

    /// Get entries as formatted string for prompt injection.
    pub fn to_prompt_content(&self) -> String {
        if self.entries.is_empty() {
            return "(no workspace learnings yet)".to_string();
        }

        let mut content = String::from("## Workspace Learnings\n\n");

        for entry in &self.entries {
            content.push_str(&format!(
                "- **{}** (bead: {}, confidence: {}): {}\n",
                entry.bead_type.as_str(),
                entry.bead_id,
                entry.confidence.as_str(),
                entry.observation
            ));
        }

        content
    }

    /// Find similar entries based on observation text.
    pub fn find_similar(&self, observation: &str) -> Vec<&LearningEntry> {
        let obs_lower = observation.to_lowercase();

        self.entries
            .iter()
            .filter(|e| {
                // Simple similarity check: look for shared words
                let entry_lower = e.observation.to_lowercase();
                let entry_words: std::collections::HashSet<&str> =
                    entry_lower.split_whitespace().collect();
                let obs_words: std::collections::HashSet<&str> =
                    obs_lower.split_whitespace().collect();

                // Count shared words
                let shared = entry_words.intersection(&obs_words).count();
                shared >= 2 || entry_lower.contains(&obs_lower)
            })
            .collect()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Retrospective
// ──────────────────────────────────────────────────────────────────────────────

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
