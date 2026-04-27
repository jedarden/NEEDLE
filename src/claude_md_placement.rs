//! CLAUDE.md placement strategy for promoted learnings.
//!
//! When a learning appears across multiple workspaces, it should be written to
//! the CLAUDE.md file at the lowest common ancestor directory covering all
//! contributing workspaces. This ensures the learning appears in the system
//! prompt only when working in relevant projects.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::learning::{LearningEntry, LearningsFile};

/// A promoted learning with its source workspaces tracked.
#[derive(Debug, Clone)]
pub struct PromotedLearning {
    /// The learning entry to promote.
    pub entry: LearningEntry,
    /// Workspaces where this pattern was observed.
    pub source_workspaces: Vec<PathBuf>,
}

impl PromotedLearning {
    /// Create a new promoted learning.
    pub fn new(entry: LearningEntry, source_workspaces: Vec<PathBuf>) -> Self {
        PromotedLearning {
            entry,
            source_workspaces,
        }
    }
}

/// Manages CLAUDE.md file placement for promoted learnings.
#[derive(Debug, Clone)]
pub struct ClaudeMdPlacer {
    /// Known workspace roots for finding ancestors.
    workspace_roots: Vec<PathBuf>,
}

impl ClaudeMdPlacer {
    /// Create a new ClaudeMdPlacer with known workspace roots.
    pub fn new(workspace_roots: Vec<PathBuf>) -> Self {
        ClaudeMdPlacer { workspace_roots }
    }

    /// Find the deepest CLAUDE.md whose directory is a parent of all contributing workspaces.
    ///
    /// Returns None if no suitable ancestor is found (shouldn't happen with valid workspace paths).
    pub fn find_target_claude_md(&self, workspaces: &[PathBuf]) -> Option<PathBuf> {
        if workspaces.is_empty() {
            return None;
        }

        // Single workspace: use that workspace's CLAUDE.md
        if workspaces.len() == 1 {
            let ws = &workspaces[0];
            let claude_md = ws.join("CLAUDE.md");
            if claude_md.exists() {
                return Some(claude_md);
            }
            // Fall through to create at workspace level
        }

        // Multiple workspaces: find the lowest common ancestor
        let lca = self.find_lowest_common_ancestor(workspaces)?;
        let claude_md = lca.join("CLAUDE.md");

        // Check if CLAUDE.md exists at this level or any parent
        let mut search_path = claude_md.clone();
        loop {
            if search_path.exists() {
                return Some(search_path);
            }
            search_path = match search_path.parent() {
                Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                _ => break,
            };
            search_path = search_path.join("CLAUDE.md");
        }

        // No existing CLAUDE.md found - return the LCA location for creation
        Some(claude_md)
    }

    /// Find the lowest common ancestor directory of all given workspaces.
    fn find_lowest_common_ancestor(&self, workspaces: &[PathBuf]) -> Option<PathBuf> {
        if workspaces.is_empty() {
            return None;
        }

        if workspaces.len() == 1 {
            return Some(workspaces[0].clone());
        }

        // Normalize all paths to absolute paths
        let abs_paths: Vec<PathBuf> = workspaces
            .iter()
            .map(|p| {
                if p.is_absolute() {
                    p.clone()
                } else {
                    std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())
                }
            })
            .collect();

        // Find common prefix
        let first = abs_paths[0].components().collect::<Vec<_>>();
        let mut common_depth = first.len();

        for path in &abs_paths[1..] {
            let components = path.components().collect::<Vec<_>>();
            let min_depth = common_depth.min(components.len());

            let mut matching = 0;
            for i in 0..min_depth {
                if first[i] == components[i] {
                    matching += 1;
                } else {
                    break;
                }
            }
            common_depth = matching;
            if common_depth == 0 {
                break;
            }
        }

        if common_depth == 0 {
            // No common ancestor - fall back to home directory
            return Some(home_dir());
        }

        // Build the common ancestor path
        let lca: PathBuf = first[..common_depth].iter().collect();
        Some(lca)
    }

    /// Write a promoted learning to the appropriate CLAUDE.md file.
    ///
    /// Creates the file with frontmatter if it doesn't exist.
    /// Avoids duplicating learnings already present in the file.
    pub fn place_learning(&self, promoted: &PromotedLearning) -> Result<bool> {
        let target = self
            .find_target_claude_md(&promoted.source_workspaces)
            .context("failed to find target CLAUDE.md")?;

        // Load existing content to check for duplicates
        let existing_content = if target.exists() {
            std::fs::read_to_string(&target)
                .with_context(|| format!("failed to read CLAUDE.md: {}", target.display()))?
        } else {
            String::new()
        };

        // Check for duplicate observation
        if self.contains_learning(&existing_content, &promoted.entry.observation) {
            tracing::debug!(
                observation = %promoted.entry.observation,
                target = %target.display(),
                "claude_md: learning already exists, skipping"
            );
            return Ok(false);
        }

        // Ensure parent directory exists
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory: {}", parent.display()))?;
        }

        // Build the new content
        let new_content = if existing_content.is_empty() {
            // Create new CLAUDE.md with frontmatter
            format!(
                "# Project Conventions\n\n\
                 ## NEEDLE Learnings\n\n\
                 {}",
                self.format_learning(promoted)
            )
        } else if existing_content.contains("## NEEDLE Learnings") {
            // Append to existing NEEDLE Learnings section
            let needle_idx = existing_content.find("## NEEDLE Learnings").unwrap();
            let before = &existing_content[..needle_idx];
            let section_start = needle_idx + "## NEEDLE Learnings".len();

            // Find the end of the NEEDLE Learnings section (next ## or end of file)
            let after_section = &existing_content[section_start..];
            let next_section_idx = after_section
                .find("\n## ")
                .unwrap_or(after_section.len());

            let section_content = &after_section[..next_section_idx];
            let after = &after_section[next_section_idx..];

            format!(
                "{}## NEEDLE Learnings\n\n{}{}{}",
                before,
                section_content,
                self.format_learning(promoted),
                after
            )
        } else {
            // Append NEEDLE Learnings section to end of file
            format!(
                "{}\n\n## NEEDLE Learnings\n\n{}",
                existing_content.trim(),
                self.format_learning(promoted)
            )
        };

        std::fs::write(&target, new_content)
            .with_context(|| format!("failed to write CLAUDE.md: {}", target.display()))?;

        tracing::info!(
            observation = %promoted.entry.observation,
            target = %target.display(),
            workspaces = ?promoted.source_workspaces,
            "claude_md: placed promoted learning"
        );

        Ok(true)
    }

    /// Format a learning entry for CLAUDE.md.
    ///
    /// Decision-type learnings use ADR-lite format with HTML comment markers.
    /// Habit-type learnings use simple bullet format.
    fn format_learning(&self, promoted: &PromotedLearning) -> String {
        // Check if this is a decision-type learning with full ADR context
        if let Some(ref ctx) = promoted.entry.decision_context {
            // Full ADR-lite format for decisions with context
            // Format matches epic spec: decision on first line, context/rationale on new lines with 2-space indent
            let mut result = format!(
                "<!-- needle-learning -->\n- **Decision**: {}\n  **Context**: {}\n  **Rationale**: {}\n",
                truncate(&ctx.decision, 150),
                truncate(&ctx.context, 200),
                truncate(&ctx.rationale, 300)
            );

            // Add alternatives if present
            if !ctx.alternatives.is_empty() {
                result.push_str(&format!(
                    "  **Alternatives**: {}\n",
                    truncate(&ctx.alternatives.join(", "), 200)
                ));
            }

            result.push_str("<!-- /needle-learning -->\n");
            result
        } else if promoted.entry.decision_id.is_some() {
            // Legacy decision format (decision_id without context)
            format!(
                "<!-- needle-learning -->\n- **Decision**: {}\n  **Context**: {}\n  **Rationale**: {}\n<!-- /needle-learning -->\n",
                truncate(&promoted.entry.observation, 150),
                "See full context in learnings.md",
                ""
            )
        } else {
            // Simple format for habit-type learnings
            format!(
                "<!-- needle-learning:{} -->\n- **{}** (bead: {}, confidence: {}): {}\n<!-- /needle-learning:{} -->\n",
                promoted.entry.bead_id,
                promoted.entry.bead_type.as_str(),
                promoted.entry.bead_id,
                promoted.entry.confidence.as_str(),
                promoted.entry.observation,
                promoted.entry.bead_id
            )
        }
    }

    /// Check if a learning with the given observation already exists in the content.
    fn contains_learning(&self, content: &str, observation: &str) -> bool {
        // Look for the observation text in NEEDLE Learnings section
        if let Some(needle_idx) = content.find("## NEEDLE Learnings") {
            let needle_section = &content[needle_idx..];
            // Find the next section header or end
            let section_end = needle_section
                .find("\n## ")
                .unwrap_or(needle_section.len());
            let section = &needle_section[..section_end];

            // Check for HTML comment markers with this learning
            // Both formats: <!-- needle-learning:ID --> and observation text
            let observation_lower = observation.to_lowercase();

            // Look for decision markers or bead ID markers
            if section.contains("<!-- needle-learning:") && section.contains("<!-- /needle-learning:") {
                // Extract learning IDs from markers
                let mut learning_ids = Vec::new();
                for part in section.split("<!-- needle-learning:") {
                    if let Some(end) = part.find(" -->") {
                        let id = part[..end].trim();
                        learning_ids.push(id);
                    }
                }

                // Check if any marker contains our observation or bead ID
                for id in learning_ids {
                    if section.contains(&format!("<!-- needle-learning:{} -->", id)) {
                        // Found a matching marker, check if the content is similar
                        let marker_start = format!("<!-- needle-learning:{} -->", id);
                        let marker_end = format!("<!-- /needle-learning:{} -->", id);
                        if let Some(start_idx) = section.find(&marker_start) {
                            if let Some(end_idx) = section.find(&marker_end) {
                                let learning_content = &section[start_idx..end_idx + marker_end.len()];
                                if learning_content.to_lowercase().contains(&observation_lower) {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }

            // Fallback: simple substring check
            section.to_lowercase().contains(&observation_lower)
        } else {
            false
        }
    }

    /// Place multiple promoted learnings, returning the number successfully placed.
    pub fn place_learnings(&self, promoted: &[PromotedLearning]) -> Result<usize> {
        let mut placed = 0;
        for p in promoted {
            if self.place_learning(p)? {
                placed += 1;
            }
        }
        Ok(placed)
    }
}

/// Get the home directory.
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Truncate a string to a maximum length, adding ellipsis if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max_len.saturating_sub(3)).collect::<String>())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Cross-workspace pattern detection with workspace tracking
// ──────────────────────────────────────────────────────────────────────────────

/// Detect patterns that appear across multiple workspaces and track source workspaces.
pub fn detect_cross_workspace_patterns(
    current_workspace: &Path,
    known_workspaces: &[PathBuf],
) -> Result<Vec<PromotedLearning>> {
    let mut promoted = Vec::new();

    // Load current workspace learnings
    let current_learnings = match LearningsFile::load(current_workspace) {
        Ok(lf) => lf,
        Err(e) => {
            tracing::warn!(
                workspace = %current_workspace.display(),
                error = %e,
                "claude_md: failed to load current workspace learnings"
            );
            return Ok(promoted);
        }
    };

    if current_learnings.entries().is_empty() {
        return Ok(promoted);
    }

    // Load all known workspace learnings
    let mut all_workspaces = vec![current_workspace.to_path_buf()];
    all_workspaces.extend(
        known_workspaces
            .iter()
            .filter(|p| p.as_path() != current_workspace)
            .cloned(),
    );

    let mut workspace_learnings: HashMap<PathBuf, Vec<LearningEntry>> = HashMap::new();

    for ws in &all_workspaces {
        match LearningsFile::load(ws) {
            Ok(lf) if !lf.entries().is_empty() => {
                workspace_learnings.insert(ws.clone(), lf.entries().to_vec());
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    workspace = %ws.display(),
                    error = %e,
                    "claude_md: failed to load learnings"
                );
            }
        }
    }

    // For each entry in current workspace, check if it appears in other workspaces
    for entry in current_learnings.entries() {
        let mut matching_workspaces = vec![current_workspace.to_path_buf()];

        for (ws, entries) in &workspace_learnings {
            if ws == current_workspace {
                continue;
            }

            // Check if any entry in this workspace is similar
            let has_similar = entries.iter().any(|e| {
                observations_similar(&e.observation, &entry.observation)
            });

            if has_similar {
                matching_workspaces.push(ws.clone());
            }
        }

        // Only promote if appears in 2+ workspaces
        if matching_workspaces.len() >= 2 {
            promoted.push(PromotedLearning::new(
                entry.clone(),
                matching_workspaces,
            ));
        }
    }

    Ok(promoted)
}

/// Simple similarity check for observation text.
pub fn observations_similar(a: &str, b: &str) -> bool {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();

    // Word overlap check
    let a_words: HashSet<&str> = a_lower.split_whitespace().collect();
    let b_words: HashSet<&str> = b_lower.split_whitespace().collect();

    let shared = a_words.intersection(&b_words).count();
    let min_len = a_words.len().min(b_words.len());

    // At least 50% word overlap or contains check
    shared >= 2 && (shared as f32) >= (min_len as f32 * 0.5)
        || a_lower.contains(&b_lower)
        || b_lower.contains(&a_lower)
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learning::{BeadType, Confidence};

    fn make_entry(observation: &str) -> LearningEntry {
        LearningEntry::new(
            "nd-test".to_string(),
            "test-worker".to_string(),
            BeadType::Other,
            observation.to_string(),
            Confidence::High,
            "test-source".to_string(),
        )
    }

    #[test]
    fn find_lca_single_workspace() {
        let placer = ClaudeMdPlacer::new(vec![]);
        let workspaces = vec![PathBuf::from("/home/coding/project1")];
        let lca = placer.find_lowest_common_ancestor(&workspaces);
        assert_eq!(lca, Some(PathBuf::from("/home/coding/project1")));
    }

    #[test]
    fn find_lca_sibling_workspaces() {
        let placer = ClaudeMdPlacer::new(vec![]);
        let workspaces = vec![
            PathBuf::from("/home/coding/project1"),
            PathBuf::from("/home/coding/project2"),
        ];
        let lca = placer.find_lowest_common_ancestor(&workspaces);
        assert_eq!(lca, Some(PathBuf::from("/home/coding")));
    }

    #[test]
    fn find_lca_nested_workspaces() {
        let placer = ClaudeMdPlacer::new(vec![]);
        let workspaces = vec![
            PathBuf::from("/home/coding/project1/src"),
            PathBuf::from("/home/coding/project1/tests"),
        ];
        let lca = placer.find_lowest_common_ancestor(&workspaces);
        assert_eq!(lca, Some(PathBuf::from("/home/coding/project1")));
    }

    #[test]
    fn find_lca_three_workspaces() {
        let placer = ClaudeMdPlacer::new(vec![]);
        let workspaces = vec![
            PathBuf::from("/home/coding/project1"),
            PathBuf::from("/home/coding/project2"),
            PathBuf::from("/home/coding/project3"),
        ];
        let lca = placer.find_lowest_common_ancestor(&workspaces);
        assert_eq!(lca, Some(PathBuf::from("/home/coding")));
    }

    #[test]
    fn find_target_single_workspace_prefers_existing_claude_md() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();

        // Create CLAUDE.md in workspace
        let claude_md = ws.join("CLAUDE.md");
        std::fs::write(&claude_md, "# Test\n").unwrap();

        let placer = ClaudeMdPlacer::new(vec![]);
        let target = placer.find_target_claude_md(&[ws.clone()]);
        assert_eq!(target, Some(claude_md));
    }

    #[test]
    fn find_target_creates_at_workspace_when_none_exist() {
        let dir = tempfile::tempdir().unwrap();
        let ws1 = dir.path().join("ws1");
        let ws2 = dir.path().join("ws2");
        std::fs::create_dir_all(&ws1).unwrap();
        std::fs::create_dir_all(&ws2).unwrap();

        let placer = ClaudeMdPlacer::new(vec![]);
        let target = placer.find_target_claude_md(&[ws1.clone(), ws2.clone()]);
        // Should point to parent directory
        assert_eq!(target, Some(dir.path().join("CLAUDE.md")));
    }

    #[test]
    fn place_learning_creates_new_claude_md() {
        let dir = tempfile::tempdir().unwrap();
        let ws1 = dir.path().join("ws1");
        let ws2 = dir.path().join("ws2");
        std::fs::create_dir_all(&ws1).unwrap();
        std::fs::create_dir_all(&ws2).unwrap();

        let placer = ClaudeMdPlacer::new(vec![]);
        let entry = make_entry("use existing patterns from modules");
        let promoted = PromotedLearning::new(entry, vec![ws1.clone(), ws2.clone()]);

        let result = placer.place_learning(&promoted);
        assert!(result.is_ok());
        assert!(result.unwrap());

        let target = dir.path().join("CLAUDE.md");
        assert!(target.exists());

        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("# Project Conventions"));
        assert!(content.contains("## NEEDLE Learnings"));
        assert!(content.contains("use existing patterns from modules"));
    }

    #[test]
    fn place_learning_appends_to_existing_section() {
        let dir = tempfile::tempdir().unwrap();
        let ws1 = dir.path().join("ws1");
        std::fs::create_dir_all(&ws1).unwrap();

        // Create existing CLAUDE.md with NEEDLE Learnings section
        let claude_md = dir.path().join("CLAUDE.md");
        std::fs::write(
            &claude_md,
            "# Project Conventions\n\n\
             ## NEEDLE Learnings\n\n\
             - **other** (bead: nd-old, confidence: high): old learning\n\n\
             ## Other Section\n\nContent here",
        )
        .unwrap();

        let placer = ClaudeMdPlacer::new(vec![]);
        let entry = make_entry("new learning pattern");
        let promoted = PromotedLearning::new(entry, vec![ws1.clone()]);

        let result = placer.place_learning(&promoted);
        assert!(result.unwrap());

        let content = std::fs::read_to_string(&claude_md).unwrap();
        assert!(content.contains("old learning"));
        assert!(content.contains("new learning pattern"));
        assert!(content.contains("## Other Section"));
    }

    #[test]
    fn place_learning_adds_section_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let ws1 = dir.path().join("ws1");
        std::fs::create_dir_all(&ws1).unwrap();

        // Create existing CLAUDE.md WITHOUT NEEDLE Learnings section
        let claude_md = dir.path().join("CLAUDE.md");
        std::fs::write(&claude_md, "# Project Conventions\n\nExisting content").unwrap();

        let placer = ClaudeMdPlacer::new(vec![]);
        let entry = make_entry("new learning pattern");
        let promoted = PromotedLearning::new(entry, vec![ws1.clone()]);

        let result = placer.place_learning(&promoted);
        assert!(result.unwrap());

        let content = std::fs::read_to_string(&claude_md).unwrap();
        assert!(content.contains("## NEEDLE Learnings"));
        assert!(content.contains("new learning pattern"));
        assert!(content.contains("Existing content"));
    }

    #[test]
    fn place_learning_skips_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let ws1 = dir.path().join("ws1");
        std::fs::create_dir_all(&ws1).unwrap();

        // Create existing CLAUDE.md with the learning
        let claude_md = dir.path().join("CLAUDE.md");
        std::fs::write(
            &claude_md,
            "# Project Conventions\n\n\
             ## NEEDLE Learnings\n\n\
             - **other** (bead: nd-old, confidence: high): use existing patterns from modules\n",
        )
        .unwrap();

        let placer = ClaudeMdPlacer::new(vec![]);
        let entry = make_entry("use existing patterns from modules");
        let promoted = PromotedLearning::new(entry, vec![ws1.clone()]);

        let result = placer.place_learning(&promoted);
        assert!(result.is_ok());
        assert!(!result.unwrap()); // Not placed (duplicate)
    }

    #[test]
    fn observations_similar_with_word_overlap() {
        assert!(observations_similar(
            "use existing pattern from modules",
            "use existing patterns from module"
        ));
        assert!(observations_similar(
            "run cargo clippy before committing",
            "run cargo clippy before commit"
        ));
    }

    #[test]
    fn observations_not_similar() {
        assert!(!observations_similar(
            "use existing pattern from modules",
            "completely unrelated observation here"
        ));
        assert!(!observations_similar("short", "other"));
    }

    #[test]
    fn detect_cross_workspace_patterns_finds_matches() {
        let dir = tempfile::tempdir().unwrap();
        let ws1 = dir.path().join("ws1");
        let ws2 = dir.path().join("ws2");
        std::fs::create_dir_all(&ws1.join(".beads")).unwrap();
        std::fs::create_dir_all(&ws2.join(".beads")).unwrap();

        // Create learnings files with matching observations
        let obs = "use existing pattern from modules";
        for ws in [&ws1, &ws2] {
            let content = format!(
                "# Workspace Learnings\n\n\
                 ### 2026-04-26 | bead: nd-test | worker: test | type: other | reinforced: 0\n\
                 - **Observation:** {}\n\
                 - **Confidence:** high\n\
                 - **Source:** test\n\n",
                obs
            );
            std::fs::write(ws.join(".beads/learnings.md"), content).unwrap();
        }

        let result = detect_cross_workspace_patterns(&ws1, &[ws2.clone()]).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_workspaces.len(), 2);
    }

    #[test]
    fn detect_cross_workspace_patterns_empty_when_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let ws1 = dir.path().join("ws1");
        let ws2 = dir.path().join("ws2");
        std::fs::create_dir_all(&ws1.join(".beads")).unwrap();
        std::fs::create_dir_all(&ws2.join(".beads")).unwrap();

        // Create learnings files with different observations
        for (ws, obs) in [
            (&ws1, "use existing pattern from modules"),
            (&ws2, "completely different observation"),
        ] {
            let content = format!(
                "# Workspace Learnings\n\n\
                 ### 2026-04-26 | bead: nd-test | worker: test | type: other | reinforced: 0\n\
                 - **Observation:** {}\n\
                 - **Confidence:** high\n\
                 - **Source:** test\n\n",
                obs
            );
            std::fs::write(ws.join(".beads/learnings.md"), content).unwrap();
        }

        let result = detect_cross_workspace_patterns(&ws1, &[ws2.clone()]).unwrap();
        assert_eq!(result.len(), 0);
    }
}
