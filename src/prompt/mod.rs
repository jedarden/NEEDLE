//! Prompt construction from bead context.
//!
//! PromptBuilder constructs a deterministic prompt string from a claimed bead.
//! The prompt instructs the agent what work to do and how to report completion.
//!
//! Depends on: `types`, `config`.

use anyhow::Result;

use crate::config::Config;
use crate::types::Bead;

/// Constructs agent prompts from bead context.
pub struct PromptBuilder {
    config: Config,
}

impl PromptBuilder {
    pub fn new(config: Config) -> Self {
        PromptBuilder { config }
    }

    /// Build the prompt for the given bead.
    ///
    /// The prompt includes:
    /// - Bead ID, title, and body
    /// - Instruction to close the bead on completion (`br close <id>`)
    /// - Any workspace-specific context from config
    pub fn build(&self, bead: &Bead) -> Result<String> {
        // TODO(needle-nva): support configurable prompt templates
        let body = bead.body.as_deref().unwrap_or("(no body)");
        let workspace = self.config.workspace.display();
        let prompt = format!(
            "You are a NEEDLE bead worker. Work in {workspace}.\n\n\
             Bead: {id}\n\
             Title: {title}\n\n\
             {body}\n\n\
             When done, run: br close {id} --body \"<summary of what was done>\"",
            workspace = workspace,
            id = bead.id,
            title = bead.title,
            body = body,
        );
        Ok(prompt)
    }
}
