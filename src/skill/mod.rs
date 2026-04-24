//! Skill library: stored procedures promoted from repeated learnings.
//!
//! Skills are stored as markdown files in `.beads/skills/` with YAML frontmatter.
//! The `SkillLibrary` loads all skills, matches them to beads by label/task-type,
//! and provides formatting for prompt injection.
//!
//! ## File format
//!
//! ```markdown
//! ---
//! task_types: [bug-fix, api-integration]
//! labels: [api, rate-limiting]
//! success_count: 7
//! last_used: "2026-04-03"
//! source_beads: [nd-a3f8, nd-b7c2]
//! ---
//! ## Skill Name
//! Procedure description with steps.
//! ```
//!
//! ## Promotion lifecycle
//!
//! 1. **Observation**: captured in `.beads/learnings.md` by the Reflect strand.
//! 2. **Promotion**: entry promoted to skill when `reinforcement_count >= 3`.
//! 3. **Use**: PromptBuilder injects top 3 matching skills into every prompt.
//! 4. **Success tracking**: `success_count` incremented on successful bead closure.
//!
//! Depends on: nothing (leaf module).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────────────
// Frontmatter
// ──────────────────────────────────────────────────────────────────────────────

/// YAML frontmatter for a skill file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillFrontmatter {
    /// Task types this skill applies to (e.g., `bug-fix`, `feature`).
    #[serde(default)]
    pub task_types: Vec<String>,
    /// Labels from beads this skill matches (e.g., `api`, `rust`).
    #[serde(default)]
    pub labels: Vec<String>,
    /// Number of times this skill was successfully used.
    #[serde(default)]
    pub success_count: u32,
    /// Date the skill was last used (YYYY-MM-DD).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
    /// Bead IDs that contributed to this skill.
    #[serde(default)]
    pub source_beads: Vec<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// SkillFile
// ──────────────────────────────────────────────────────────────────────────────

/// A parsed skill file.
#[derive(Debug, Clone)]
pub struct SkillFile {
    /// The path to the skill file on disk.
    pub path: PathBuf,
    /// Parsed YAML frontmatter.
    pub frontmatter: SkillFrontmatter,
    /// The markdown body after the frontmatter delimiters.
    pub body: String,
}

impl SkillFile {
    /// Parse a skill file from raw content and path.
    pub fn parse(path: PathBuf, content: &str) -> Result<Self> {
        let (frontmatter, body) = parse_frontmatter(content)?;
        Ok(SkillFile {
            path,
            frontmatter,
            body,
        })
    }

    /// Compute a match score against a bead's labels and title.
    ///
    /// Returns 0 if no fields match. Higher scores indicate a better match.
    /// Scoring: +2 per matching label, +1 per matching task_type keyword in title.
    pub fn match_score(&self, labels: &[String], title: &str) -> u32 {
        let mut score = 0u32;
        let title_lower = title.to_lowercase();

        for label in labels {
            let label_lower = label.to_lowercase();
            // Exact label match against skill labels.
            if self
                .frontmatter
                .labels
                .iter()
                .any(|l| l.to_lowercase() == label_lower)
            {
                score += 2;
            }
            // Label match against task_types.
            if self
                .frontmatter
                .task_types
                .iter()
                .any(|t| t.to_lowercase() == label_lower)
            {
                score += 1;
            }
        }

        // Task type keyword presence in title.
        for task_type in &self.frontmatter.task_types {
            let tt_lower = task_type.to_lowercase();
            let tt_spaces = tt_lower.replace('-', " ");
            if title_lower.contains(&tt_lower) || title_lower.contains(&tt_spaces) {
                score += 1;
            }
        }

        score
    }

    /// Returns the first heading from the body as the skill name, or a fallback.
    pub fn name(&self) -> &str {
        for line in self.body.lines() {
            let trimmed = line.trim_start_matches('#').trim();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
        self.path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("(unnamed skill)")
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// SkillLibrary
// ──────────────────────────────────────────────────────────────────────────────

/// Manages the collection of skill files in `.beads/skills/`.
#[derive(Debug)]
pub struct SkillLibrary {
    skills: Vec<SkillFile>,
    skills_dir: PathBuf,
}

impl SkillLibrary {
    /// Load all skill files from `<workspace>/.beads/skills/`.
    ///
    /// Missing or empty skills directory is not an error — returns an empty library.
    pub fn load(workspace: &Path) -> Result<Self> {
        let skills_dir = workspace.join(".beads").join("skills");
        let mut skills = Vec::new();

        if !skills_dir.exists() {
            return Ok(SkillLibrary { skills, skills_dir });
        }

        let entries = std::fs::read_dir(&skills_dir)
            .with_context(|| format!("failed to read skills dir: {}", skills_dir.display()))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(content) => match SkillFile::parse(path.clone(), &content) {
                    Ok(skill) => skills.push(skill),
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "failed to parse skill file");
                    }
                },
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to read skill file");
                }
            }
        }

        Ok(SkillLibrary { skills, skills_dir })
    }

    /// Return the top 3 skills matching the given bead labels and title.
    ///
    /// Matching is based on label overlap and task_type keywords. At equal score,
    /// skills with a higher `success_count` are preferred. Returns at most 3 results.
    pub fn matching_skills<'a>(&'a self, labels: &[String], title: &str) -> Vec<&'a SkillFile> {
        let mut scored: Vec<(u32, &SkillFile)> = self
            .skills
            .iter()
            .filter_map(|skill| {
                let score = skill.match_score(labels, title);
                if score > 0 {
                    Some((score, skill))
                } else {
                    None
                }
            })
            .collect();

        // Sort by score desc, then success_count desc as tiebreaker.
        scored.sort_by_key(|(score, skill)| {
            std::cmp::Reverse((*score, skill.frontmatter.success_count))
        });

        scored.into_iter().take(3).map(|(_, skill)| skill).collect()
    }

    /// Format a list of matched skills as a prompt section.
    ///
    /// Returns an empty string if `skills` is empty.
    pub fn to_prompt_content(skills: &[&SkillFile]) -> String {
        if skills.is_empty() {
            return String::new();
        }

        let mut lines = vec![
            "### Relevant Skills".to_string(),
            String::new(),
            "The following proven procedures are relevant to this task:".to_string(),
            String::new(),
        ];

        for (i, skill) in skills.iter().enumerate() {
            lines.push(format!(
                "**Skill {}:** {} (used {} times)",
                i + 1,
                skill.name(),
                skill.frontmatter.success_count
            ));
            lines.push(String::new());
            for line in skill.body.trim().lines() {
                lines.push(line.to_string());
            }
            lines.push(String::new());
        }

        lines.join("\n")
    }

    /// Increment `success_count` and update `last_used` for the skill at `path`.
    pub fn increment_success(path: &Path) -> Result<()> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read skill file: {}", path.display()))?;

        let (mut frontmatter, body) = parse_frontmatter(&content)?;
        frontmatter.success_count += 1;
        frontmatter.last_used = Some(Utc::now().format("%Y-%m-%d").to_string());

        let new_content = render_skill_file(&frontmatter, &body)?;
        std::fs::write(path, new_content)
            .with_context(|| format!("failed to write skill file: {}", path.display()))?;

        Ok(())
    }

    /// Increment `success_count` for all skills matching the given bead labels and title.
    pub fn increment_success_for_bead(&self, labels: &[String], title: &str) -> Result<()> {
        let matching = self.matching_skills(labels, title);
        for skill in matching {
            if let Err(e) = Self::increment_success(&skill.path) {
                tracing::warn!(
                    path = %skill.path.display(),
                    error = %e,
                    "failed to increment skill success count"
                );
            }
        }
        Ok(())
    }

    /// Return the set of source bead IDs found across all loaded skill files.
    ///
    /// Used by the Reflect strand to avoid re-promoting the same learning.
    pub fn promoted_source_beads(&self) -> HashSet<String> {
        let mut ids = HashSet::new();
        for skill in &self.skills {
            for bead_id in &skill.frontmatter.source_beads {
                ids.insert(bead_id.clone());
            }
        }
        ids
    }

    /// Whether the library is empty.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Number of skills loaded.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// The skills directory path.
    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }

    /// Create an empty skill library with no backing directory.
    pub fn new_empty() -> Self {
        SkillLibrary {
            skills: Vec::new(),
            skills_dir: PathBuf::new(),
        }
    }

    /// Load skills from another workspace and merge those whose labels overlap
    /// with `workspace_labels` into this library.
    ///
    /// This enables cross-workspace skill sharing: skills tagged with generic
    /// labels (e.g., `rust`, `api`) are made available to any workspace whose
    /// `workspace.labels` list contains a matching label.
    ///
    /// Skills already present (by path) are skipped to avoid duplicates.
    pub fn extend_from_workspace(&mut self, workspace: &Path, workspace_labels: &[String]) {
        if workspace_labels.is_empty() {
            return;
        }

        let skills_dir = workspace.join(".beads").join("skills");
        if !skills_dir.exists() {
            return;
        }

        let entries = match std::fs::read_dir(&skills_dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    path = %skills_dir.display(),
                    error = %e,
                    "failed to read cross-workspace skills dir"
                );
                return;
            }
        };

        let ws_labels_lower: HashSet<String> =
            workspace_labels.iter().map(|l| l.to_lowercase()).collect();

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            // Skip if already loaded (e.g., same path already in local skills).
            if self.skills.iter().any(|s| s.path == path) {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(content) => match SkillFile::parse(path.clone(), &content) {
                    Ok(skill) => {
                        // Include only if any skill label matches a workspace label.
                        let matches = skill
                            .frontmatter
                            .labels
                            .iter()
                            .any(|l| ws_labels_lower.contains(&l.to_lowercase()));
                        if matches {
                            self.skills.push(skill);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to parse cross-workspace skill file"
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to read cross-workspace skill file"
                    );
                }
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Frontmatter parsing and rendering
// ──────────────────────────────────────────────────────────────────────────────

/// Parse YAML frontmatter from a markdown file.
///
/// The file must begin with `---\n`. If no frontmatter is present, returns
/// default frontmatter and the full content as the body.
pub fn parse_frontmatter(content: &str) -> Result<(SkillFrontmatter, String)> {
    if !content.starts_with("---\n") && !content.starts_with("---\r\n") {
        return Ok((SkillFrontmatter::default(), content.to_string()));
    }

    // Skip the opening `---\n`.
    let after_open = if let Some(stripped) = content.strip_prefix("---\r\n") {
        stripped
    } else {
        &content[4..]
    };

    // Locate the closing `---` delimiter (must be on its own line).
    let end = after_open
        .find("\n---\n")
        .or_else(|| after_open.find("\n---\r\n"))
        .or_else(|| {
            // Closing `---` at end of file (no trailing newline after).
            if after_open.ends_with("\n---") {
                Some(after_open.len() - 4)
            } else {
                None
            }
        })
        .ok_or_else(|| anyhow::anyhow!("unclosed frontmatter block (missing closing ---)"))
        .with_context(|| "parsing skill file frontmatter")?;

    let yaml_str = &after_open[..end];

    // Body starts after `\n---\n` (or `\n---\r\n`).
    let body_start = if after_open[end..].starts_with("\n---\r\n") {
        end + 6
    } else if after_open[end..].starts_with("\n---\n") {
        end + 5
    } else {
        after_open.len() // closing `---` at EOF
    };

    let body = after_open.get(body_start..).unwrap_or("").to_string();

    let frontmatter: SkillFrontmatter =
        serde_yaml::from_str(yaml_str).with_context(|| "failed to parse skill frontmatter YAML")?;

    Ok((frontmatter, body))
}

/// Render a skill file with updated frontmatter and body.
pub fn render_skill_file(frontmatter: &SkillFrontmatter, body: &str) -> Result<String> {
    let yaml = serde_yaml::to_string(frontmatter)
        .with_context(|| "failed to serialize skill frontmatter")?;
    // serde_yaml::to_string produces a trailing newline; prepend the opening delimiter.
    Ok(format!("---\n{}---\n{}", yaml, body))
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_skill_content(
        task_types: &[&str],
        labels: &[&str],
        success_count: u32,
        body: &str,
    ) -> String {
        let fm = SkillFrontmatter {
            task_types: task_types.iter().map(|s| s.to_string()).collect(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            success_count,
            last_used: None,
            source_beads: vec!["nd-test".to_string()],
        };
        render_skill_file(&fm, body).unwrap()
    }

    #[test]
    fn parse_roundtrip() {
        let content = make_skill_content(
            &["bug-fix"],
            &["rust", "api"],
            3,
            "## Fix Pattern\n\nDo the thing.\n",
        );
        let (fm, body) = parse_frontmatter(&content).unwrap();
        assert_eq!(fm.task_types, vec!["bug-fix"]);
        assert_eq!(fm.labels, vec!["rust", "api"]);
        assert_eq!(fm.success_count, 3);
        assert_eq!(fm.source_beads, vec!["nd-test"]);
        assert!(body.contains("Do the thing."));
    }

    #[test]
    fn parse_no_frontmatter() {
        let content = "## Old Format\n\nNo YAML here.\n";
        let (fm, body) = parse_frontmatter(content).unwrap();
        assert!(fm.task_types.is_empty());
        assert!(fm.labels.is_empty());
        assert_eq!(fm.success_count, 0);
        assert!(body.contains("No YAML here."));
    }

    #[test]
    fn skill_file_name_from_heading() {
        let content = make_skill_content(&[], &[], 0, "## My Skill\n\nDo it.\n");
        let skill = SkillFile::parse(PathBuf::from("test.md"), &content).unwrap();
        assert_eq!(skill.name(), "My Skill");
    }

    #[test]
    fn skill_file_name_fallback() {
        let skill = SkillFile {
            path: PathBuf::from("my-skill.md"),
            frontmatter: SkillFrontmatter::default(),
            body: String::new(),
        };
        assert_eq!(skill.name(), "my-skill");
    }

    #[test]
    fn match_score_label_match() {
        let content = make_skill_content(&["feature"], &["rust", "api"], 0, "## Skill\n");
        let skill = SkillFile::parse(PathBuf::from("s.md"), &content).unwrap();
        let labels = vec!["rust".to_string(), "ci".to_string()];
        let score = skill.match_score(&labels, "add endpoint");
        assert_eq!(score, 2); // one label match (+2)
    }

    #[test]
    fn match_score_task_type_in_title() {
        let content = make_skill_content(&["bug-fix"], &[], 0, "## Fix\n");
        let skill = SkillFile::parse(PathBuf::from("s.md"), &content).unwrap();
        let labels: Vec<String> = vec![];
        let score = skill.match_score(&labels, "bug fix in parser");
        assert_eq!(score, 1); // task type keyword in title (+1)
    }

    #[test]
    fn match_score_no_match() {
        let content = make_skill_content(&["feature"], &["api"], 0, "## Skill\n");
        let skill = SkillFile::parse(PathBuf::from("s.md"), &content).unwrap();
        let labels = vec!["rust".to_string()];
        let score = skill.match_score(&labels, "database migration");
        assert_eq!(score, 0);
    }

    #[test]
    fn library_matching_skills_top_3() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".beads").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        // Write 4 skills, 3 matching rust label.
        for (i, (success, labels)) in [
            (5u32, vec!["rust"]),
            (2u32, vec!["rust"]),
            (8u32, vec!["rust"]),
            (1u32, vec!["python"]),
        ]
        .iter()
        .enumerate()
        {
            let content = make_skill_content(&["feature"], labels, *success, "## Skill\n");
            std::fs::write(skills_dir.join(format!("skill-{i}.md")), content).unwrap();
        }

        let lib = SkillLibrary::load(dir.path()).unwrap();
        let bead_labels = vec!["rust".to_string()];
        let matches = lib.matching_skills(&bead_labels, "add feature");

        // Should return 3 matching skills (python one excluded), sorted by success_count desc.
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].frontmatter.success_count, 8);
        assert_eq!(matches[1].frontmatter.success_count, 5);
        assert_eq!(matches[2].frontmatter.success_count, 2);
    }

    #[test]
    fn library_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let lib = SkillLibrary::load(dir.path()).unwrap();
        assert!(lib.is_empty());
        assert_eq!(lib.len(), 0);
    }

    #[test]
    fn to_prompt_content_empty() {
        let content = SkillLibrary::to_prompt_content(&[]);
        assert!(content.is_empty());
    }

    #[test]
    fn to_prompt_content_with_skills() {
        let raw = make_skill_content(&["bug-fix"], &["rust"], 3, "## Fix\n\nUse pattern X.\n");
        let skill = SkillFile::parse(PathBuf::from("s.md"), &raw).unwrap();
        let content = SkillLibrary::to_prompt_content(&[&skill]);
        assert!(content.contains("Relevant Skills"));
        assert!(content.contains("Skill 1:"));
        assert!(content.contains("used 3 times"));
        assert!(content.contains("Use pattern X."));
    }

    #[test]
    fn increment_success_updates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skill.md");
        let content = make_skill_content(&["feature"], &["rust"], 0, "## Skill\n");
        std::fs::write(&path, content).unwrap();

        SkillLibrary::increment_success(&path).unwrap();

        let updated = std::fs::read_to_string(&path).unwrap();
        let (fm, _) = parse_frontmatter(&updated).unwrap();
        assert_eq!(fm.success_count, 1);
        assert!(fm.last_used.is_some());
    }

    #[test]
    fn promoted_source_beads() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".beads").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let fm = SkillFrontmatter {
            task_types: vec![],
            labels: vec![],
            success_count: 0,
            last_used: None,
            source_beads: vec!["nd-abc1".to_string(), "nd-def2".to_string()],
        };
        let content = render_skill_file(&fm, "## S\n").unwrap();
        std::fs::write(skills_dir.join("test.md"), content).unwrap();

        let lib = SkillLibrary::load(dir.path()).unwrap();
        let ids = lib.promoted_source_beads();
        assert!(ids.contains("nd-abc1"));
        assert!(ids.contains("nd-def2"));
    }

    #[test]
    fn extend_from_workspace_adds_matching_skills() {
        let _local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();

        // Remote workspace has two skills: one matching [rust], one not.
        let remote_skills = remote_dir.path().join(".beads").join("skills");
        std::fs::create_dir_all(&remote_skills).unwrap();

        let rust_skill = make_skill_content(&["feature"], &["rust", "api"], 5, "## Rust Skill\n");
        let go_skill = make_skill_content(&["feature"], &["go"], 3, "## Go Skill\n");
        std::fs::write(remote_skills.join("rust.md"), &rust_skill).unwrap();
        std::fs::write(remote_skills.join("go.md"), &go_skill).unwrap();

        // Local library is initially empty.
        let mut lib = SkillLibrary::new_empty();

        // Workspace labels include "rust" — should import the rust skill only.
        let workspace_labels = vec!["rust".to_string(), "trading".to_string()];
        lib.extend_from_workspace(remote_dir.path(), &workspace_labels);

        assert_eq!(lib.len(), 1);
        assert_eq!(
            lib.matching_skills(&["rust".to_string()], "add feature")
                .len(),
            1
        );
    }

    #[test]
    fn extend_from_workspace_no_labels_is_noop() {
        let remote_dir = tempfile::tempdir().unwrap();
        let remote_skills = remote_dir.path().join(".beads").join("skills");
        std::fs::create_dir_all(&remote_skills).unwrap();
        let skill = make_skill_content(&["feature"], &["rust"], 1, "## S\n");
        std::fs::write(remote_skills.join("s.md"), skill).unwrap();

        let mut lib = SkillLibrary::new_empty();
        lib.extend_from_workspace(remote_dir.path(), &[]);
        assert!(lib.is_empty());
    }

    #[test]
    fn extend_from_workspace_skips_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".beads").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        let skill = make_skill_content(&["feature"], &["rust"], 2, "## S\n");
        std::fs::write(skills_dir.join("s.md"), &skill).unwrap();

        // Load local skills, then extend from the same workspace — no duplicates.
        let mut lib = SkillLibrary::load(dir.path()).unwrap();
        assert_eq!(lib.len(), 1);

        lib.extend_from_workspace(dir.path(), &["rust".to_string()]);
        assert_eq!(lib.len(), 1); // still 1, not 2
    }

    #[test]
    fn extend_from_workspace_cross_workspace_ranking() {
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();

        let local_skills = local_dir.path().join(".beads").join("skills");
        let remote_skills = remote_dir.path().join(".beads").join("skills");
        std::fs::create_dir_all(&local_skills).unwrap();
        std::fs::create_dir_all(&remote_skills).unwrap();

        // Local skill: success_count=2
        let local = make_skill_content(&["feature"], &["rust"], 2, "## Local\n");
        std::fs::write(local_skills.join("local.md"), local).unwrap();

        // Remote skill: success_count=9 (higher — should rank first)
        let remote = make_skill_content(&["feature"], &["rust"], 9, "## Remote\n");
        std::fs::write(remote_skills.join("remote.md"), remote).unwrap();

        let mut lib = SkillLibrary::load(local_dir.path()).unwrap();
        lib.extend_from_workspace(remote_dir.path(), &["rust".to_string()]);
        assert_eq!(lib.len(), 2);

        let matches = lib.matching_skills(&["rust".to_string()], "add feature");
        assert_eq!(matches.len(), 2);
        // Remote skill ranks first due to higher success_count.
        assert_eq!(matches[0].frontmatter.success_count, 9);
        assert_eq!(matches[1].frontmatter.success_count, 2);
    }
}
