//! Prompt construction from bead context.
//!
//! `PromptBuilder` constructs a deterministic prompt string from a claimed bead.
//! Same bead state + same config always produces the identical prompt, making
//! prompt hashes useful for telemetry and reproducibility.
//!
//! Depends on: `types`, `config`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::config::PromptConfig;
use crate::types::Bead;

// ──────────────────────────────────────────────────────────────────────────────
// Default pluck template
// ──────────────────────────────────────────────────────────────────────────────

/// Built-in pluck template used when no override is configured.
const DEFAULT_PLUCK_TEMPLATE: &str = "\
## Task

{bead_title}

## Description

{bead_body}

## Workspace

{workspace_path}

## Context Files

{context_file_contents}

## Instructions

{workspace_instructions}

Complete the task described above. When finished:
- Commit your changes with a descriptive message
- Close the bead: `br close {bead_id} --body \"Summary of what was done\"`

If you cannot complete the task:
- Do NOT close the bead
- The bead will be automatically released for retry

Bead ID: {bead_id}";

/// Built-in mitosis analysis template.
///
/// The agent receives the bead context and must output JSON describing whether
/// the bead contains multiple independent tasks and, if so, proposed child beads.
const DEFAULT_MITOSIS_TEMPLATE: &str = "\
## Mitosis Analysis

Analyze the following bead and determine if it describes MULTIPLE INDEPENDENT TASKS.

### Bead

**Title:** {bead_title}

**Description:**
{bead_body}

**Bead ID:** {bead_id}

### Instructions

You must output ONLY a JSON object (no markdown fencing, no explanation).

If the bead describes multiple independent tasks that can be worked on separately:
{{\"splittable\": true, \"children\": [{{\"title\": \"Short task title\", \"body\": \"Task description and acceptance criteria\"}}, ...]}}

If the bead describes a single task (even if complex or long):
{{\"splittable\": false}}

### Rules for splitting

- Split ONLY if the bead asks for MORE THAN ONE independent unit of work
- Each child must be independently completable and closable
- Valid split: \"add endpoint AND write migration AND update tests\" (three deliverables)
- Invalid split: bead is long, bead failed, bead has many acceptance criteria for one task
- Preserve the original acceptance criteria by distributing them to the appropriate child
- Each child title should be concise and start with a verb";

// ──────────────────────────────────────────────────────────────────────────────
// BuiltPrompt
// ──────────────────────────────────────────────────────────────────────────────

/// The rendered output of prompt construction.
#[derive(Debug, Clone)]
pub struct BuiltPrompt {
    /// The fully rendered prompt string.
    pub content: String,
    /// SHA-256 hex digest of `content` (for telemetry reproducibility).
    pub hash: String,
    /// Rough token estimate (chars / 4).
    pub token_estimate: u64,
}

// ──────────────────────────────────────────────────────────────────────────────
// PromptBuilder
// ──────────────────────────────────────────────────────────────────────────────

/// Constructs agent prompts from bead context.
///
/// Designed with named templates in mind (Phase 3 adds per-strand overrides),
/// but Phase 1 only uses the `pluck` template.
pub struct PromptBuilder {
    /// Named templates. Key `"pluck"` is always present.
    templates: BTreeMap<String, String>,
    /// Paths to context files (relative to the workspace root).
    context_file_paths: Vec<std::path::PathBuf>,
    /// Free-form workspace instructions appended to prompts.
    workspace_instructions: Option<String>,
}

impl PromptBuilder {
    /// Create a new `PromptBuilder` from prompt config.
    pub fn new(config: &PromptConfig) -> Self {
        let mut templates = BTreeMap::new();
        templates.insert("pluck".to_string(), DEFAULT_PLUCK_TEMPLATE.to_string());
        templates.insert("mitosis".to_string(), DEFAULT_MITOSIS_TEMPLATE.to_string());

        PromptBuilder {
            templates,
            context_file_paths: config.context_files.clone(),
            workspace_instructions: config.instructions.clone(),
        }
    }

    /// Build the prompt for a claimed bead using the named template.
    ///
    /// # Arguments
    /// * `bead` — The claimed bead to build a prompt for.
    /// * `workspace` — Absolute path to the workspace directory.
    /// * `worker_id` — Identifier of the current worker.
    /// * `template_name` — Which named template to use (e.g., `"pluck"`).
    ///
    /// Returns `Err` only for truly unrecoverable errors (e.g., template not found).
    /// Missing context files are silently omitted.
    pub fn build(
        &self,
        bead: &Bead,
        workspace: &Path,
        worker_id: &str,
        template_name: &str,
    ) -> Result<BuiltPrompt> {
        let template = self
            .templates
            .get(template_name)
            .with_context(|| format!("unknown prompt template: {template_name}"))?;

        let context_file_contents = self.load_context_files(workspace);
        let instructions = self
            .workspace_instructions
            .as_deref()
            .unwrap_or("(no workspace instructions)");
        let body = bead.body.as_deref().unwrap_or("(no description)");

        let content = template
            .replace("{bead_id}", bead.id.as_ref())
            .replace("{bead_title}", &bead.title)
            .replace("{bead_body}", body)
            .replace("{workspace_path}", &workspace.display().to_string())
            .replace("{context_file_contents}", &context_file_contents)
            .replace("{workspace_instructions}", instructions)
            .replace("{worker_id}", worker_id);

        let hash = hex_sha256(&content);
        let token_estimate = content.len() as u64 / 4;

        Ok(BuiltPrompt {
            content,
            hash,
            token_estimate,
        })
    }

    /// Convenience method that uses the default `"pluck"` template.
    pub fn build_pluck(
        &self,
        bead: &Bead,
        workspace: &Path,
        worker_id: &str,
    ) -> Result<BuiltPrompt> {
        self.build(bead, workspace, worker_id, "pluck")
    }

    /// Load and concatenate context files from the workspace.
    ///
    /// Each file is prefixed with a header showing the file path.
    /// Missing files are silently skipped.
    fn load_context_files(&self, workspace: &Path) -> String {
        if self.context_file_paths.is_empty() {
            return "(no context files configured)".to_string();
        }

        let mut sections = Vec::new();
        for rel_path in &self.context_file_paths {
            let abs_path = workspace.join(rel_path);
            match std::fs::read_to_string(&abs_path) {
                Ok(contents) => {
                    sections.push(format!(
                        "### {}\n\n{}",
                        rel_path.display(),
                        contents.trim_end()
                    ));
                }
                Err(_) => {
                    // Silently omit missing files per spec.
                }
            }
        }

        if sections.is_empty() {
            "(no context files found)".to_string()
        } else {
            sections.join("\n\n")
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Compute the SHA-256 hex digest of a string.
fn hex_sha256(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let result = hasher.finalize();
    // Format as lowercase hex
    result.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PromptConfig;
    use crate::types::{BeadId, BeadStatus};
    use chrono::Utc;
    use std::path::PathBuf;

    fn test_bead() -> Bead {
        Bead {
            id: BeadId::from("needle-abc"),
            title: "Implement the widget".to_string(),
            body: Some("Build a widget that does things.".to_string()),
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: Some("worker-01".to_string()),
            labels: vec![],
            workspace: PathBuf::from("/tmp/test-workspace"),
            dependencies: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn build_pluck_contains_bead_id() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_pluck(&bead, Path::new("/tmp/test-workspace"), "worker-01")
            .unwrap();

        assert!(
            result.content.contains("needle-abc"),
            "prompt must contain bead ID"
        );
    }

    #[test]
    fn build_pluck_contains_close_instruction() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_pluck(&bead, Path::new("/tmp/test-workspace"), "worker-01")
            .unwrap();

        assert!(
            result.content.contains("br close needle-abc"),
            "prompt must contain br close instruction"
        );
    }

    #[test]
    fn build_pluck_contains_title_and_body() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_pluck(&bead, Path::new("/tmp/test-workspace"), "worker-01")
            .unwrap();

        assert!(result.content.contains("Implement the widget"));
        assert!(result.content.contains("Build a widget that does things."));
    }

    #[test]
    fn deterministic_same_inputs_same_output() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let ws = Path::new("/tmp/test-workspace");

        let a = builder.build_pluck(&bead, ws, "worker-01").unwrap();
        let b = builder.build_pluck(&bead, ws, "worker-01").unwrap();

        assert_eq!(a.content, b.content, "same inputs must produce same prompt");
        assert_eq!(a.hash, b.hash, "same inputs must produce same hash");
        assert_eq!(a.token_estimate, b.token_estimate);
    }

    #[test]
    fn hash_is_valid_hex_sha256() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_pluck(&bead, Path::new("/tmp/test-workspace"), "worker-01")
            .unwrap();

        assert_eq!(result.hash.len(), 64, "SHA-256 hex digest is 64 chars");
        assert!(
            result.hash.chars().all(|c| c.is_ascii_hexdigit()),
            "hash must be valid hex"
        );
    }

    #[test]
    fn token_estimate_is_reasonable() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_pluck(&bead, Path::new("/tmp/test-workspace"), "worker-01")
            .unwrap();

        let expected = result.content.len() as u64 / 4;
        assert_eq!(result.token_estimate, expected);
        assert!(result.token_estimate > 0, "token estimate must be positive");
    }

    #[test]
    fn no_literal_template_variables_in_output() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_pluck(&bead, Path::new("/tmp/test-workspace"), "worker-01")
            .unwrap();

        let variables = [
            "{bead_id}",
            "{bead_title}",
            "{bead_body}",
            "{workspace_path}",
            "{context_file_contents}",
            "{workspace_instructions}",
            "{worker_id}",
        ];
        for var in &variables {
            assert!(
                !result.content.contains(var),
                "literal template variable {var} should not appear in output"
            );
        }
    }

    #[test]
    fn missing_body_uses_fallback() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let mut bead = test_bead();
        bead.body = None;
        let result = builder
            .build_pluck(&bead, Path::new("/tmp/test-workspace"), "worker-01")
            .unwrap();

        assert!(
            result.content.contains("(no description)"),
            "missing body should show fallback"
        );
    }

    #[test]
    fn missing_context_files_do_not_error() {
        let config = PromptConfig {
            context_files: vec![
                PathBuf::from("DOES_NOT_EXIST.md"),
                PathBuf::from("ALSO_MISSING.md"),
            ],
            instructions: None,
        };
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_pluck(&bead, Path::new("/tmp/test-workspace"), "worker-01")
            .unwrap();

        // Should not error, and should indicate no files found
        assert!(
            result.content.contains("(no context files found)"),
            "missing files should produce fallback text"
        );
    }

    #[test]
    fn context_files_are_loaded_when_present() {
        // Create a temp dir with a context file
        let dir = std::env::temp_dir().join("needle-prompt-test");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("AGENTS.md"), "# Agent instructions\nDo good work.").unwrap();

        let config = PromptConfig {
            context_files: vec![PathBuf::from("AGENTS.md")],
            instructions: Some("Always run tests.".to_string()),
        };
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder.build_pluck(&bead, &dir, "worker-01").unwrap();

        assert!(
            result.content.contains("Agent instructions"),
            "context file content should appear in prompt"
        );
        assert!(
            result.content.contains("AGENTS.md"),
            "context file path header should appear"
        );
        assert!(
            result.content.contains("Always run tests."),
            "workspace instructions should appear"
        );

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_template_returns_error() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder.build(&bead, Path::new("/tmp"), "w1", "nonexistent");

        assert!(result.is_err(), "unknown template name should error");
    }

    #[test]
    fn workspace_path_appears_in_output() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_pluck(&bead, Path::new("/home/coding/myproject"), "worker-01")
            .unwrap();

        assert!(result.content.contains("/home/coding/myproject"));
    }

    #[test]
    fn worker_id_appears_in_output() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_pluck(&bead, Path::new("/tmp"), "needle-w42")
            .unwrap();

        // worker_id is substituted but only appears if the template references it.
        // The default pluck template does not reference {worker_id} in visible text,
        // but the substitution should still happen without error.
        assert!(!result.content.contains("{worker_id}"));
    }

    #[test]
    fn hex_sha256_known_value() {
        // SHA-256 of "" is e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let hash = hex_sha256("");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
