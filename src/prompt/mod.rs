//! Prompt construction from bead context.
//!
//! `PromptBuilder` constructs a deterministic prompt string from a claimed bead.
//! Same bead state + same config always produces the identical prompt, making
//! prompt hashes useful for telemetry and reproducibility.
//!
//! All agent-invoking operations (Pluck, Mitosis, Weave, Unravel, Pulse) use
//! named templates. Templates are configurable per-workspace and globally.
//!
//! Depends on: `types`, `config`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::config::PromptConfig;
use crate::types::Bead;

// ──────────────────────────────────────────────────────────────────────────────
// Default templates
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

### Existing Children

{existing_children}

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
- Each child title should be concise and start with a verb
- Do not propose children that duplicate any existing children listed above";

/// Built-in weave template for gap analysis and bead creation.
const DEFAULT_WEAVE_TEMPLATE: &str = "\
## Workspace Documentation

{doc_files}

## Current Open Beads

{existing_beads}

## Question

Review the documentation above. Identify gaps where documented features, \
APIs, or workflows are incomplete, missing tests, or have no corresponding \
implementation bead.

For each gap found, propose a bead with:
- title: concise description of what's missing
- body: what needs to be done to close the gap
- priority: 1 (critical), 2 (important), or 3 (nice-to-have)

Do not propose beads that duplicate any existing open beads listed above.
If no gaps are found, respond with: NO_GAPS";

/// Built-in unravel template for proposing alternatives to HUMAN-blocked beads.
const DEFAULT_UNRAVEL_TEMPLATE: &str = "\
## Blocked Bead

Title: {bead_title}
Body: {bead_body}
Status: Blocked (requires human decision)

## Context

{human_bead_context}

## Question

This bead is blocked because it requires a human decision. Analyze the bead \
and propose alternative approaches that could be executed by an automated \
agent without the human decision.

For each alternative, provide:
- title: concise description of the alternative approach
- body: what would be done differently
- tradeoffs: what is gained and what is lost compared to the original approach

If no viable alternatives exist, respond with: NO_ALTERNATIVES";

/// Built-in pulse template for health scan bead creation.
const DEFAULT_PULSE_TEMPLATE: &str = "\
## Scan Results

{scan_results}

## Current Open Beads

{existing_beads}

## Question

Review the scan results above. For issues that are significant enough to \
warrant a fix, propose a bead with:
- title: concise description of the issue
- body: what needs to be fixed and how
- priority: based on severity (1=critical, 2=important, 3=minor)

Do not propose beads that duplicate any existing open beads listed above.
If no significant issues are found, respond with: NO_ISSUES";

// ──────────────────────────────────────────────────────────────────────────────
// Known template names and their allowed variables
// ──────────────────────────────────────────────────────────────────────────────

/// Common variables available to all templates.
const COMMON_VARS: &[&str] = &[
    "{bead_id}",
    "{bead_title}",
    "{bead_body}",
    "{workspace_path}",
    "{context_file_contents}",
    "{workspace_instructions}",
    "{worker_id}",
];

/// Returns the extra (strand-specific) variables allowed for a given template name.
/// Returns `None` for unknown template names.
fn extra_vars_for_template(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "pluck" => Some(&[]),
        "mitosis" => Some(&["{existing_children}"]),
        "weave" => Some(&["{doc_files}", "{existing_beads}"]),
        "unravel" => Some(&["{human_bead_context}"]),
        "pulse" => Some(&["{scan_results}", "{existing_beads}"]),
        _ => None,
    }
}

/// All known built-in template names.
const KNOWN_TEMPLATE_NAMES: &[&str] = &["pluck", "mitosis", "weave", "unravel", "pulse"];

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
/// All five named templates (`pluck`, `mitosis`, `weave`, `unravel`, `pulse`)
/// are always present with built-in defaults. User-provided overrides from
/// config replace specific templates while others keep their defaults.
pub struct PromptBuilder {
    /// Named templates. All five built-in names are always present.
    templates: BTreeMap<String, String>,
    /// Paths to context files (relative to the workspace root).
    context_file_paths: Vec<std::path::PathBuf>,
    /// Free-form workspace instructions appended to prompts.
    workspace_instructions: Option<String>,
}

impl PromptBuilder {
    /// Create a new `PromptBuilder` from prompt config.
    ///
    /// Initializes all built-in templates, then overrides any that are
    /// specified in `config.templates`.
    pub fn new(config: &PromptConfig) -> Self {
        let mut templates = BTreeMap::new();
        templates.insert("pluck".to_string(), DEFAULT_PLUCK_TEMPLATE.to_string());
        templates.insert("mitosis".to_string(), DEFAULT_MITOSIS_TEMPLATE.to_string());
        templates.insert("weave".to_string(), DEFAULT_WEAVE_TEMPLATE.to_string());
        templates.insert("unravel".to_string(), DEFAULT_UNRAVEL_TEMPLATE.to_string());
        templates.insert("pulse".to_string(), DEFAULT_PULSE_TEMPLATE.to_string());

        // Apply user overrides (partial: only specified templates are replaced).
        for (name, body) in &config.templates {
            templates.insert(name.clone(), body.clone());
        }

        PromptBuilder {
            templates,
            context_file_paths: config.context_files.clone(),
            workspace_instructions: config.instructions.clone(),
        }
    }

    /// Validate all templates at boot time.
    ///
    /// For each known template, checks that every `{variable}` reference in the
    /// template body is a recognized variable. Returns an error listing all
    /// invalid references found.
    pub fn validate(&self) -> Result<()> {
        let mut errors = Vec::new();

        for (name, body) in &self.templates {
            let allowed_extra = extra_vars_for_template(name);

            // Extract all {variable} references from the template body.
            // Skip escaped braces ({{ and }}) used for literal JSON.
            let vars = extract_template_vars(body);

            for var in vars {
                let is_common = COMMON_VARS.contains(&var.as_str());
                let is_extra = allowed_extra
                    .map(|extras| extras.contains(&var.as_str()))
                    .unwrap_or(false);

                if !is_common && !is_extra {
                    errors.push(format!("template \"{name}\": unknown variable {var}"));
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            bail!("invalid prompt template(s):\n  {}", errors.join("\n  "));
        }
    }

    /// Build the prompt for a claimed bead using the named template.
    ///
    /// Common variables (`{bead_id}`, `{bead_title}`, etc.) are always substituted.
    /// Missing context files are silently omitted.
    pub fn build(
        &self,
        bead: &Bead,
        workspace: &Path,
        worker_id: &str,
        template_name: &str,
    ) -> Result<BuiltPrompt> {
        self.build_with_vars(bead, workspace, worker_id, template_name, &[])
    }

    /// Build the prompt with additional strand-specific variables.
    ///
    /// `extra_vars` is a slice of `(variable_name, value)` pairs for
    /// strand-specific substitutions (e.g., `("{doc_files}", "...")` for weave).
    pub fn build_with_vars(
        &self,
        bead: &Bead,
        workspace: &Path,
        worker_id: &str,
        template_name: &str,
        extra_vars: &[(&str, &str)],
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

        // Substitute common variables.
        let mut content = template
            .replace("{bead_id}", bead.id.as_ref())
            .replace("{bead_title}", &bead.title)
            .replace("{bead_body}", body)
            .replace("{workspace_path}", &workspace.display().to_string())
            .replace("{context_file_contents}", &context_file_contents)
            .replace("{workspace_instructions}", instructions)
            .replace("{worker_id}", worker_id);

        // Substitute strand-specific variables.
        for (var, value) in extra_vars {
            content = content.replace(var, value);
        }

        let hash = hex_sha256(&content);
        let token_estimate = content.len() as u64 / 4;

        Ok(BuiltPrompt {
            content,
            hash,
            token_estimate,
        })
    }

    /// Convenience method that uses the `"pluck"` template.
    pub fn build_pluck(
        &self,
        bead: &Bead,
        workspace: &Path,
        worker_id: &str,
    ) -> Result<BuiltPrompt> {
        self.build(bead, workspace, worker_id, "pluck")
    }

    /// Build a weave (gap analysis) prompt.
    ///
    /// `doc_files` — formatted listing of documentation files and contents.
    /// `existing_beads` — formatted listing of current open beads.
    pub fn build_weave(
        &self,
        bead: &Bead,
        workspace: &Path,
        worker_id: &str,
        doc_files: &str,
        existing_beads: &str,
    ) -> Result<BuiltPrompt> {
        self.build_with_vars(
            bead,
            workspace,
            worker_id,
            "weave",
            &[
                ("{doc_files}", doc_files),
                ("{existing_beads}", existing_beads),
            ],
        )
    }

    /// Build an unravel (alternative proposals) prompt.
    ///
    /// `human_bead_context` — context about the HUMAN-blocked bead being analyzed.
    pub fn build_unravel(
        &self,
        bead: &Bead,
        workspace: &Path,
        worker_id: &str,
        human_bead_context: &str,
    ) -> Result<BuiltPrompt> {
        self.build_with_vars(
            bead,
            workspace,
            worker_id,
            "unravel",
            &[("{human_bead_context}", human_bead_context)],
        )
    }

    /// Build a pulse (health scan) prompt.
    ///
    /// `scan_results` — output from configured scanners.
    /// `existing_beads` — formatted listing of current open beads.
    pub fn build_pulse(
        &self,
        bead: &Bead,
        workspace: &Path,
        worker_id: &str,
        scan_results: &str,
        existing_beads: &str,
    ) -> Result<BuiltPrompt> {
        self.build_with_vars(
            bead,
            workspace,
            worker_id,
            "pulse",
            &[
                ("{scan_results}", scan_results),
                ("{existing_beads}", existing_beads),
            ],
        )
    }

    /// Build a mitosis (split analysis) prompt.
    ///
    /// `existing_children` — formatted listing of the parent's current children.
    pub fn build_mitosis(
        &self,
        bead: &Bead,
        workspace: &Path,
        worker_id: &str,
        existing_children: &str,
    ) -> Result<BuiltPrompt> {
        self.build_with_vars(
            bead,
            workspace,
            worker_id,
            "mitosis",
            &[("{existing_children}", existing_children)],
        )
    }

    /// Returns an iterator over all template names.
    pub fn template_names(&self) -> impl Iterator<Item = &str> {
        self.templates.keys().map(|s| s.as_str())
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

/// Extract all `{variable}` references from a template string.
///
/// Skips escaped braces (`{{` and `}}`) which are used for literal JSON.
/// Returns unique variable references including the braces (e.g., `"{bead_id}"`).
fn extract_template_vars(template: &str) -> Vec<String> {
    let mut vars = Vec::new();
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
            // Find matching closing brace.
            if let Some(end) = chars[i + 1..].iter().position(|&c| c == '}') {
                let end = i + 1 + end;
                let var_name: String = chars[i..=end].iter().collect();
                // Only include if it looks like a valid variable (alphanumeric + underscore).
                let inner: String = chars[i + 1..end].iter().collect();
                if !inner.is_empty()
                    && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !vars.contains(&var_name)
                {
                    vars.push(var_name);
                }
                i = end + 1;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    vars
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
            templates: BTreeMap::new(),
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
            templates: BTreeMap::new(),
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

    // ── New template tests ──────────────────────────────────────────────

    #[test]
    fn all_five_default_templates_present() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let names: Vec<&str> = builder.template_names().collect();

        for expected in KNOWN_TEMPLATE_NAMES {
            assert!(
                names.contains(expected),
                "default templates must include \"{expected}\""
            );
        }
    }

    #[test]
    fn build_weave_substitutes_doc_files() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_weave(
                &bead,
                Path::new("/tmp"),
                "w1",
                "README.md contents here",
                "needle-001: open task",
            )
            .unwrap();

        assert!(result.content.contains("README.md contents here"));
        assert!(result.content.contains("needle-001: open task"));
        assert!(!result.content.contains("{doc_files}"));
        assert!(!result.content.contains("{existing_beads}"));
    }

    #[test]
    fn build_unravel_substitutes_human_context() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_unravel(
                &bead,
                Path::new("/tmp"),
                "w1",
                "Blocked on architecture decision for auth",
            )
            .unwrap();

        assert!(result
            .content
            .contains("Blocked on architecture decision for auth"));
        assert!(!result.content.contains("{human_bead_context}"));
    }

    #[test]
    fn build_pulse_substitutes_scan_results() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_pulse(
                &bead,
                Path::new("/tmp"),
                "w1",
                "clippy: 3 warnings found",
                "needle-xyz: existing bead",
            )
            .unwrap();

        assert!(result.content.contains("clippy: 3 warnings found"));
        assert!(result.content.contains("needle-xyz: existing bead"));
        assert!(!result.content.contains("{scan_results}"));
        assert!(!result.content.contains("{existing_beads}"));
    }

    #[test]
    fn build_mitosis_substitutes_existing_children() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_mitosis(&bead, Path::new("/tmp"), "w1", "needle-c1: child task one")
            .unwrap();

        assert!(result.content.contains("needle-c1: child task one"));
        assert!(!result.content.contains("{existing_children}"));
    }

    #[test]
    fn config_template_override_replaces_default() {
        let mut templates = BTreeMap::new();
        templates.insert(
            "pluck".to_string(),
            "Custom: {bead_title} in {workspace_path}".to_string(),
        );

        let config = PromptConfig {
            context_files: vec![],
            instructions: None,
            templates,
        };
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder.build_pluck(&bead, Path::new("/tmp"), "w1").unwrap();

        assert!(result.content.starts_with("Custom: Implement the widget"));
        // The default "## Task" header should NOT appear
        assert!(!result.content.contains("## Task"));
    }

    #[test]
    fn partial_override_keeps_other_defaults() {
        let mut templates = BTreeMap::new();
        templates.insert("pluck".to_string(), "Custom pluck".to_string());

        let config = PromptConfig {
            context_files: vec![],
            instructions: None,
            templates,
        };
        let builder = PromptBuilder::new(&config);

        // pluck was overridden
        let bead = test_bead();
        let pluck = builder.build_pluck(&bead, Path::new("/tmp"), "w1").unwrap();
        assert_eq!(pluck.content, "Custom pluck");

        // weave still uses default
        let weave = builder
            .build_weave(&bead, Path::new("/tmp"), "w1", "docs", "beads")
            .unwrap();
        assert!(weave.content.contains("Workspace Documentation"));
    }

    #[test]
    fn validate_default_templates_pass() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        builder
            .validate()
            .expect("default templates should be valid");
    }

    #[test]
    fn validate_catches_unknown_variable() {
        let mut templates = BTreeMap::new();
        templates.insert(
            "pluck".to_string(),
            "Hello {bead_title} {unknown_var}".to_string(),
        );

        let config = PromptConfig {
            context_files: vec![],
            instructions: None,
            templates,
        };
        let builder = PromptBuilder::new(&config);
        let err = builder.validate();

        assert!(err.is_err(), "unknown variable should fail validation");
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("unknown_var"),
            "error should name the variable"
        );
    }

    #[test]
    fn validate_allows_strand_specific_vars_in_correct_template() {
        let mut templates = BTreeMap::new();
        templates.insert(
            "weave".to_string(),
            "Docs: {doc_files} Beads: {existing_beads} Title: {bead_title}".to_string(),
        );

        let config = PromptConfig {
            context_files: vec![],
            instructions: None,
            templates,
        };
        let builder = PromptBuilder::new(&config);
        builder
            .validate()
            .expect("strand-specific vars should be valid in their template");
    }

    #[test]
    fn validate_rejects_strand_specific_var_in_wrong_template() {
        let mut templates = BTreeMap::new();
        // {doc_files} is only valid in weave, not pluck
        templates.insert("pluck".to_string(), "{bead_title} {doc_files}".to_string());

        let config = PromptConfig {
            context_files: vec![],
            instructions: None,
            templates,
        };
        let builder = PromptBuilder::new(&config);
        let err = builder.validate();

        assert!(err.is_err(), "doc_files in pluck template should fail");
    }

    #[test]
    fn extract_vars_skips_escaped_braces() {
        let template = r#"JSON: {{"key": "value"}} and {bead_id}"#;
        let vars = extract_template_vars(template);
        assert_eq!(vars, vec!["{bead_id}"]);
    }

    #[test]
    fn extract_vars_handles_empty() {
        let vars = extract_template_vars("no variables here");
        assert!(vars.is_empty());
    }

    #[test]
    fn extract_vars_deduplicates() {
        let vars = extract_template_vars("{bead_id} and {bead_id} again");
        assert_eq!(vars, vec!["{bead_id}"]);
    }

    #[test]
    fn build_with_vars_extra_substitution() {
        let config = PromptConfig::default();
        let builder = PromptBuilder::new(&config);
        let bead = test_bead();
        let result = builder
            .build_with_vars(
                &bead,
                Path::new("/tmp"),
                "w1",
                "weave",
                &[("{doc_files}", "MY_DOCS"), ("{existing_beads}", "MY_BEADS")],
            )
            .unwrap();

        assert!(result.content.contains("MY_DOCS"));
        assert!(result.content.contains("MY_BEADS"));
    }
}
