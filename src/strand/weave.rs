//! Weave strand: gap analysis and bead creation from documentation.
//!
//! When all other strands (Pluck, Mend, Explore) returned NoWork,
//! Weave analyzes workspace documentation for gaps and creates beads
//! to address them.
//!
//! Heavily guardrailed (from v1 lessons):
//! - **Opt-in only.** Disabled by default.
//! - **Max beads per run.** Configurable, default 5.
//! - **Cooldown.** Minimum hours between runs, default 24h.
//! - **Dedup.** Tracks previously created titles to prevent duplicates.
//! - **Workspace exclusion.** Configurable list of forbidden workspaces.
//! - **Weave-generated label.** All created beads are labeled for filtering.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use libc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bead_store::BeadStore;
use crate::config::{Config, ExploreConfig, GenerationConfig, WeaveConfig};
use crate::dispatch::AgentAdapter;
use crate::process_guard::ProcessGroupKillGuard;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{BeadId, InputMethod, StrandResult};

/// Keep generated prompts comfortably below model context limits after the
/// agent adds repository instructions and other harness context of its own.
const MAX_WEAVE_DOC_CONTEXT_BYTES: usize = 32 * 1024;
const MAX_WEAVE_DOC_FILE_BYTES: usize = 4 * 1024;
const MAX_WEAVE_BEAD_CONTEXT_BYTES: usize = 16 * 1024;
const CONTEXT_TRUNCATION_MARKER: &str = "\n\n[context truncated]\n";

// ─── WeaveAgent trait ────────────────────────────────────────────────────────

/// Abstraction for agent invocation used by the Weave strand.
///
/// Production implementations wrap the `Dispatcher`; tests use mocks.
#[async_trait::async_trait]
pub trait WeaveAgent: Send + Sync {
    /// Invoke an agent with the given prompt, returning its raw text response.
    async fn analyze_gaps(&self, prompt: &str, workspace: &Path) -> Result<String>;
}

// ─── Proposed bead (parsed from agent response) ──────────────────────────────

/// A bead proposed by the agent during gap analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedBead {
    pub title: String,
    pub body: String,
    pub priority: u8,
}

// ─── Persistent state ────────────────────────────────────────────────────────

/// Persisted state for a workspace's weave runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WeaveState {
    /// Timestamp of the last weave run.
    pub last_run: Option<DateTime<Utc>>,
    /// Titles of beads previously created by weave (for dedup).
    pub seen_titles: HashSet<String>,
}

impl WeaveState {
    /// Load state from disk, returning default if file doesn't exist.
    fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(data) => serde_json::from_str(&data)
                .with_context(|| format!("failed to parse weave state: {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => {
                Err(e).with_context(|| format!("failed to read weave state: {}", path.display()))
            }
        }
    }

    /// Persist state to disk.
    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create state dir: {}", parent.display()))?;
        }
        let data = serde_json::to_string_pretty(self).context("failed to serialize weave state")?;
        std::fs::write(path, data)
            .with_context(|| format!("failed to write weave state: {}", path.display()))
    }

    /// Check if cooldown has elapsed since last run.
    fn cooldown_elapsed(&self, cooldown_hours: u64) -> bool {
        match self.last_run {
            None => true,
            Some(last) => {
                let elapsed = Utc::now().signed_duration_since(last);
                elapsed.num_hours() >= cooldown_hours as i64
            }
        }
    }

    /// Check if a title was already seen (dedup).
    fn is_duplicate(&self, title: &str) -> bool {
        self.seen_titles.contains(&title.to_lowercase())
    }

    /// Record a title as seen.
    fn mark_seen(&mut self, title: &str) {
        self.seen_titles.insert(title.to_lowercase());
    }
}

// ─── WeaveStrand ─────────────────────────────────────────────────────────────

/// The Weave strand — analyzes documentation gaps and creates beads.
pub struct WeaveStrand {
    config: WeaveConfig,
    workspace: PathBuf,
    state_dir: PathBuf,
    agent: Arc<dyn WeaveAgent>,
    telemetry: Telemetry,
    /// Low-water backlog policy. `None` keeps the strand's ordinary cooldown
    /// behaviour, which is what the default constructor gives tests and
    /// callers that do not opt into generation.
    generation: Option<super::generation::GeneratorGate>,
    /// Fleet-targeted Weave only runs under a low-water permit. This prevents
    /// a roaming worker from applying the ordinary per-workspace cooldown to
    /// every repository it can reach after Explore finds no work.
    low_water_only: bool,
}

impl WeaveStrand {
    /// Create a new WeaveStrand.
    ///
    /// `state_dir` is the base directory for weave state files
    /// (e.g., `~/.needle/state/weave/`).
    pub fn new(
        config: WeaveConfig,
        workspace: PathBuf,
        state_dir: PathBuf,
        agent: Box<dyn WeaveAgent>,
        telemetry: Telemetry,
    ) -> Self {
        Self::new_shared(config, workspace, state_dir, Arc::from(agent), telemetry)
    }

    fn new_shared(
        config: WeaveConfig,
        workspace: PathBuf,
        state_dir: PathBuf,
        agent: Arc<dyn WeaveAgent>,
        telemetry: Telemetry,
    ) -> Self {
        WeaveStrand {
            config,
            workspace,
            state_dir,
            agent,
            telemetry,
            generation: None,
            low_water_only: false,
        }
    }

    /// Enable low-water backlog generation for this strand.
    ///
    /// When the fleet's eligible-ready count falls below the configured
    /// reserve, an acquired lease lets Weave bypass its own cooldown and
    /// replenish the backlog instead of letting the cycle end in an alert.
    pub fn with_generation(
        mut self,
        config: crate::config::GenerationConfig,
        exclude_labels: Vec<String>,
    ) -> Self {
        self.generation = Some(super::generation::GeneratorGate::new(
            config,
            self.workspace.clone(),
            self.state_dir.clone(),
            exclude_labels,
            self.telemetry.clone(),
        ));
        self
    }

    /// Require a low-water permit before doing any analysis.
    fn low_water_only(mut self) -> Self {
        self.low_water_only = true;
        self
    }

    /// Compute the state file path for a workspace.
    ///
    /// Uses a SHA-256 hash of the workspace path to create a unique filename.
    fn state_file_path(&self) -> PathBuf {
        let hash = workspace_hash(&self.workspace);
        self.state_dir.join(format!("{hash}.json"))
    }

    /// Check if this workspace is excluded from weave.
    fn is_workspace_excluded(&self) -> bool {
        self.config
            .exclude_workspaces
            .iter()
            .any(|excluded| excluded == &self.workspace)
    }

    /// Discover documentation files in the workspace using configured patterns.
    fn discover_doc_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for pattern in &self.config.doc_patterns {
            let full_pattern = self.workspace.join(pattern).display().to_string();
            if let Ok(entries) = glob::glob(&full_pattern) {
                for entry in entries.flatten() {
                    if entry.is_file() && !files.contains(&entry) {
                        files.push(entry);
                    }
                }
            }
        }
        files.sort();
        files
    }

    /// Format documentation files into a bounded string for the prompt.
    ///
    /// `docs/**/*` can include generated reports measured in megabytes. A
    /// creator only needs representative planning context, so cap both each
    /// file and the aggregate instead of sending a request the model must
    /// reject before doing useful work.
    fn format_doc_files(files: &[PathBuf], workspace: &Path) -> String {
        if files.is_empty() {
            return "(no documentation files found)".to_string();
        }

        let mut output = String::new();
        for file in files {
            let rel_path = file
                .strip_prefix(workspace)
                .unwrap_or(file)
                .display()
                .to_string();
            match std::fs::read_to_string(file) {
                Ok(content) => {
                    let content = bounded_context(
                        content.trim_end(),
                        MAX_WEAVE_DOC_FILE_BYTES,
                        CONTEXT_TRUNCATION_MARKER,
                    );
                    let separator = if output.is_empty() { "" } else { "\n\n" };
                    let section = format!("{separator}### {rel_path}\n\n{content}");
                    let remaining = MAX_WEAVE_DOC_CONTEXT_BYTES.saturating_sub(output.len());
                    if section.len() > remaining {
                        output.push_str(&bounded_context(
                            &section,
                            remaining,
                            CONTEXT_TRUNCATION_MARKER,
                        ));
                        break;
                    }
                    output.push_str(&section);
                }
                Err(_) => {
                    let separator = if output.is_empty() { "" } else { "\n\n" };
                    let section = format!("{separator}### {rel_path}\n\n(failed to read)");
                    if output.len() + section.len() > MAX_WEAVE_DOC_CONTEXT_BYTES {
                        break;
                    }
                    output.push_str(&section);
                }
            }
        }
        output
    }

    /// Format existing beads into a bounded string for the prompt.
    fn format_existing_beads(beads: &[crate::types::Bead]) -> String {
        if beads.is_empty() {
            return "(no open beads)".to_string();
        }

        let mut output = String::new();
        for (index, bead) in beads.iter().enumerate() {
            let separator = if output.is_empty() { "" } else { "\n" };
            let line = format!(
                "{separator}- [{}] P{}: {}",
                bead.id.as_ref(),
                bead.priority,
                bead.title
            );
            if output.len() + line.len() > MAX_WEAVE_BEAD_CONTEXT_BYTES {
                let omitted = beads.len() - index;
                let marker = format!("\n... {omitted} additional open beads omitted");
                let remaining = MAX_WEAVE_BEAD_CONTEXT_BYTES.saturating_sub(output.len());
                output.push_str(&bounded_context(&marker, remaining, ""));
                break;
            }
            output.push_str(&line);
        }
        output
    }

    /// Parse the agent response into proposed beads.
    ///
    /// The agent may return:
    /// - `NO_GAPS` — no gaps found
    /// - A JSON array of proposed beads
    /// - A JSON object with a `beads` field containing an array
    pub fn parse_agent_response(response: &str) -> Result<Vec<ProposedBead>> {
        let trimmed = response.trim();

        // Check for NO_GAPS sentinel.
        if trimmed.contains("NO_GAPS") {
            return Ok(vec![]);
        }

        // Try parsing as a JSON array directly.
        if let Ok(beads) = serde_json::from_str::<Vec<ProposedBead>>(trimmed) {
            return Ok(beads);
        }

        // Try extracting JSON from markdown code fences.
        let json_str = extract_json_block(trimmed).unwrap_or(trimmed);

        // Try as array.
        if let Ok(beads) = serde_json::from_str::<Vec<ProposedBead>>(json_str) {
            return Ok(beads);
        }

        // Try as object with "beads" field.
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(json_str) {
            if let Some(arr) = obj.get("beads").and_then(|v| v.as_array()) {
                let beads: Vec<ProposedBead> = arr
                    .iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                    .collect();
                if !beads.is_empty() {
                    return Ok(beads);
                }
            }
        }

        anyhow::bail!("failed to parse agent response as proposed beads")
    }

    /// Build the weave prompt from discovered docs and existing beads.
    ///
    /// Uses `config.prompt_template` when set; otherwise falls back to the
    /// built-in template. Template variables: `{doc_files}`, `{existing_beads}`,
    /// `{workspace}`.
    fn build_prompt(&self, doc_files: &str, existing_beads: &str) -> String {
        if let Some(template) = &self.config.prompt_template {
            return template
                .replace("{doc_files}", doc_files)
                .replace("{existing_beads}", existing_beads)
                .replace("{workspace}", &self.workspace.display().to_string());
        }

        // Built-in default template.
        format!(
            "## Workspace Documentation\n\n\
             {doc_files}\n\n\
             ## Current Open Beads\n\n\
             {existing_beads}\n\n\
             ## Question\n\n\
             Review the documentation above. Identify gaps where documented features, \
             APIs, or workflows are incomplete, missing tests, or have no corresponding \
             implementation bead.\n\n\
             Use only the supplied context. Do not inspect files, call tools, or modify the \
             workspace; return the answer directly.\n\n\
             For each gap found, propose a bead with:\n\
             - title: concise description of what's missing\n\
             - body: what needs to be done to close the gap\n\
             - priority: 1 (critical), 2 (important), or 3 (nice-to-have)\n\n\
             Output a JSON array of objects with \"title\", \"body\", and \"priority\" fields.\n\
             Do not propose beads that duplicate any existing open beads listed above.\n\
             If no gaps are found, respond with: NO_GAPS"
        )
    }
}

/// Truncate at a UTF-8 boundary while reserving room for an explicit marker.
fn bounded_context(value: &str, max_bytes: usize, marker: &str) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let marker = if marker.len() <= max_bytes {
        marker
    } else {
        ""
    };
    let mut end = max_bytes.saturating_sub(marker.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = String::with_capacity(max_bytes);
    bounded.push_str(&value[..end]);
    bounded.push_str(marker);
    bounded
}

/// Extract a JSON block from markdown-fenced content.
fn extract_json_block(text: &str) -> Option<&str> {
    // Look for ```json ... ``` or ``` ... ```
    let start_markers = ["```json\n", "```json\r\n", "```\n", "```\r\n"];
    for marker in &start_markers {
        if let Some(start) = text.find(marker) {
            let content_start = start + marker.len();
            if let Some(end) = text[content_start..].find("```") {
                return Some(&text[content_start..content_start + end]);
            }
        }
    }
    None
}

/// Compute a short SHA-256 hash of a workspace path (for state filenames).
fn workspace_hash(workspace: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(workspace.display().to_string().as_bytes());
    let result = hasher.finalize();
    // Use first 16 hex chars for a short but unique filename.
    result
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

impl WeaveStrand {
    /// Internal evaluation logic (wrapped with timeout by evaluate).
    async fn evaluate_internal(
        &self,
        store: &dyn BeadStore,
        exclusions: &HashSet<BeadId>,
    ) -> StrandResult {
        // Guard: disabled.
        if !self.config.enabled {
            let _ = self.telemetry.emit(
                EventKind::StrandSkipped {
                    strand_name: "weave".to_string(),
                    reason: "disabled".to_string(),
                },
                Utc::now(),
            );
            tracing::debug!("weave strand disabled");
            return StrandResult::NoWork;
        }

        // Check if the home bead store exists. If not, skip this strand.
        // This distinguishes between "no home store configured" (expected for
        // roam-only workers) and "home store is broken" (unexpected error).
        if !store.has_valid_store() {
            tracing::info!(
                "Home workspace has no .beads/ directory — skipping Weave strand \
                 (expected for roam-only workers)"
            );
            return StrandResult::Skipped {
                reason: "no_home_store".to_string(),
            };
        }

        // Guard: workspace exclusion.
        if self.is_workspace_excluded() {
            let _ = self.telemetry.emit(
                EventKind::StrandSkipped {
                    strand_name: "weave".to_string(),
                    reason: "workspace_excluded".to_string(),
                },
                Utc::now(),
            );
            tracing::debug!(
                workspace = %self.workspace.display(),
                "weave strand: workspace excluded"
            );
            return StrandResult::NoWork;
        }

        // Guard: cooldown. The low-water generation gate can override it when
        // the fleet's eligible-ready count has fallen below the reserve.
        let generation_permit = match &self.generation {
            Some(gate) => Some(gate.prepare("weave", store, exclusions).await),
            None => None,
        };
        let bypass_cooldown = generation_permit
            .as_ref()
            .is_some_and(|permit| permit.bypasses_cooldown());

        // A contended lease means another empty worker is already filling this
        // gap, so generating here would duplicate the same plan gap. Continue
        // the waterfall instead and leave the alert to the terminal verdict.
        if matches!(
            generation_permit,
            Some(super::generation::GeneratorPermit::Contended)
        ) {
            return StrandResult::Skipped {
                reason: "generation_lease_contended".to_string(),
            };
        }

        if self.low_water_only
            && !matches!(
                generation_permit,
                Some(super::generation::GeneratorPermit::LowWater { .. })
            )
        {
            return StrandResult::Skipped {
                reason: "generation_not_low_water".to_string(),
            };
        }

        let state_path = self.state_file_path();
        let mut state = match WeaveState::load(&state_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load weave state, using defaults");
                WeaveState::default()
            }
        };

        if !bypass_cooldown && !state.cooldown_elapsed(self.config.cooldown_hours) {
            let _ = self.telemetry.emit(
                EventKind::StrandSkipped {
                    strand_name: "weave".to_string(),
                    reason: format!(
                        "cooldown ({}/{}/{}h)",
                        state
                            .last_run
                            .map(|t| (Utc::now() - t).num_hours())
                            .unwrap_or(-1),
                        0,
                        self.config.cooldown_hours
                    ),
                },
                Utc::now(),
            );
            tracing::debug!(
                last_run = ?state.last_run,
                cooldown_hours = self.config.cooldown_hours,
                "weave strand: cooldown not elapsed"
            );
            return StrandResult::NoWork;
        }

        // Discover documentation files.
        let doc_files = self.discover_doc_files();
        if doc_files.is_empty() {
            let _ = self.telemetry.emit(
                EventKind::StrandSkipped {
                    strand_name: "weave".to_string(),
                    reason: "no_documentation_files".to_string(),
                },
                Utc::now(),
            );
            tracing::debug!("weave strand: no documentation files found");
            return StrandResult::NoWork;
        }
        let doc_content = Self::format_doc_files(&doc_files, &self.workspace);

        // Query existing beads for dedup context with additional safety timeout.
        // The underlying list_all has its own 30s timeout, but we add an extra layer
        // of protection here to prevent any single operation from blocking indefinitely.
        let list_timeout = std::time::Duration::from_secs(45); // Give headroom above the 30s internal timeout
        let existing_beads = match tokio::time::timeout(list_timeout, store.list_all()).await {
            Ok(Ok(beads)) => beads,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "weave strand: failed to list existing beads");
                return StrandResult::Error(crate::types::StrandError::StoreError(e));
            }
            Err(_) => {
                tracing::warn!(
                    "weave strand: list_all timed out after {}s - returning NoWork to prevent stall",
                    list_timeout.as_secs()
                );
                return StrandResult::NoWork;
            }
        };
        let existing_context = Self::format_existing_beads(&existing_beads);

        // Build prompt and dispatch agent.
        let prompt = self.build_prompt(&doc_content, &existing_context);
        let response = match self.agent.analyze_gaps(&prompt, &self.workspace).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "weave strand: agent dispatch failed");
                self.emit_creator_failed(&e.to_string());
                if let (
                    Some(gate),
                    Some(super::generation::GeneratorPermit::LowWater { fencing_token }),
                ) = (&self.generation, &generation_permit)
                {
                    gate.mark_creator_failed("weave", fencing_token);
                }
                return StrandResult::Error(crate::types::StrandError::StoreError(e));
            }
        };

        // Parse proposed beads.
        let proposed = match Self::parse_agent_response(&response) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "weave strand: failed to parse agent response");
                // Update last_run even on parse failure to avoid rapid retries.
                state.last_run = Some(Utc::now());
                let _ = state.save(&state_path);
                return StrandResult::NoWork;
            }
        };

        if proposed.is_empty() {
            tracing::info!("weave strand: agent found no gaps");
            state.last_run = Some(Utc::now());
            let _ = state.save(&state_path);
            return StrandResult::NoWork;
        }

        // Create beads (with guardrails).
        let mut created = 0u32;
        let existing_titles: HashSet<String> = existing_beads
            .iter()
            .map(|b| b.title.to_lowercase())
            .collect();

        for proposed_bead in &proposed {
            // Guard: max beads per run.
            if created >= self.config.max_beads_per_run {
                tracing::info!(
                    max = self.config.max_beads_per_run,
                    "weave strand: max beads per run reached"
                );
                break;
            }

            // Guard: dedup against seen titles.
            if state.is_duplicate(&proposed_bead.title) {
                tracing::debug!(
                    title = proposed_bead.title,
                    "weave strand: skipping duplicate (seen before)"
                );
                continue;
            }

            // Guard: dedup against existing beads.
            if existing_titles.contains(&proposed_bead.title.to_lowercase()) {
                tracing::debug!(
                    title = proposed_bead.title,
                    "weave strand: skipping duplicate (already exists)"
                );
                state.mark_seen(&proposed_bead.title);
                continue;
            }

            // Clamp priority to valid range.
            let priority = proposed_bead.priority.clamp(1, 3);

            // Create the bead with weave-generated label.
            let body = format!(
                "{}\n\n---\nPriority: P{priority}\nCreated by: weave strand",
                proposed_bead.body
            );
            match store
                .create_bead(&proposed_bead.title, &body, &["weave-generated"])
                .await
            {
                Ok(bead_id) => {
                    tracing::info!(
                        bead_id = bead_id.as_ref(),
                        title = proposed_bead.title,
                        "weave strand: created bead"
                    );
                    state.mark_seen(&proposed_bead.title);
                    created += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        title = proposed_bead.title,
                        "weave strand: failed to create bead"
                    );
                }
            }
        }

        // Update state.
        state.last_run = Some(Utc::now());
        if let Err(e) = state.save(&state_path) {
            tracing::warn!(error = %e, "weave strand: failed to save state");
        }

        if created > 0 {
            tracing::info!(
                created,
                "weave strand: created beads from documentation gaps"
            );
            self.emit_work_created(created);
            StrandResult::WorkCreated
        } else {
            tracing::info!("weave strand: no new beads created (all duplicates or filtered)");
            StrandResult::NoWork
        }
    }

    /// Record that this run replenished the backlog with real work.
    fn emit_work_created(&self, created: u32) {
        let _ = self.telemetry.emit(
            EventKind::GenerationWorkCreated {
                strand_name: "weave".to_string(),
                workspace: self.workspace.display().to_string(),
                detail: format!("{created} bead(s) created from documentation gaps"),
            },
            Utc::now(),
        );
    }

    /// Record a recoverable generator failure. The waterfall still falls
    /// through to the next generator, so this must not fail the cycle.
    fn emit_creator_failed(&self, error: &str) {
        let _ = self.telemetry.emit(
            EventKind::GenerationCreatorFailed {
                strand_name: "weave".to_string(),
                workspace: self.workspace.display().to_string(),
                error: error.to_string(),
            },
            Utc::now(),
        );
    }
}

#[async_trait::async_trait]
impl super::Strand for WeaveStrand {
    fn name(&self) -> &str {
        "weave"
    }

    fn is_generator(&self) -> bool {
        true
    }

    async fn evaluate(&self, store: &dyn BeadStore, exclusions: &HashSet<BeadId>) -> StrandResult {
        // Apply strand-level timeout to prevent a single weave from stalling
        // the entire SELECTING cycle for minutes. See: needle-bf-5hlhn
        let timeout_duration = std::time::Duration::from_secs(WEAVE_STRAND_TIMEOUT_SECS);

        match tokio::time::timeout(timeout_duration, self.evaluate_internal(store, exclusions))
            .await
        {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(
                    timeout_secs = WEAVE_STRAND_TIMEOUT_SECS,
                    "weave strand evaluation timed out after {}s - returning NoWork to prevent SELECTING stall",
                    WEAVE_STRAND_TIMEOUT_SECS
                );
                let _ = self.telemetry.emit(
                    EventKind::StrandSkipped {
                        strand_name: "weave".to_string(),
                        reason: format!("timeout after {}s", WEAVE_STRAND_TIMEOUT_SECS),
                    },
                    Utc::now(),
                );
                StrandResult::NoWork
            }
        }
    }
}

// ─── CLI agent implementation ────────────────────────────────────────────────

/// Workspace-aware Weave for workers that can roam through Explore.
///
/// Explore owns selection across the configured workspace set. When that set
/// is empty, this strand walks the same approved targets and lets the
/// per-workspace generation gate choose one low-water repository to replenish.
/// A worker invokes the agent at most once per selection cycle; healthy and
/// contended targets are skipped without consuming model capacity.
pub struct FleetWeaveStrand {
    config: WeaveConfig,
    explore: ExploreConfig,
    state_dir: PathBuf,
    agent: Arc<dyn WeaveAgent>,
    telemetry: Telemetry,
    generation: GenerationConfig,
    exclude_labels: Vec<String>,
    qualified_id: String,
}

impl FleetWeaveStrand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: WeaveConfig,
        explore: ExploreConfig,
        state_dir: PathBuf,
        agent: Box<dyn WeaveAgent>,
        telemetry: Telemetry,
        generation: GenerationConfig,
        exclude_labels: Vec<String>,
        qualified_id: String,
    ) -> Self {
        Self {
            config,
            explore,
            state_dir,
            agent: Arc::from(agent),
            telemetry,
            generation,
            exclude_labels,
            qualified_id,
        }
    }

    /// Return the same workspace scope Explore is allowed to scan, reduced to
    /// structurally healthy, unique repositories and rotated per worker. The
    /// rotation spreads simultaneous empty workers across different targets;
    /// the durable generation lease remains the final duplicate-work guard.
    fn targets(&self) -> Vec<PathBuf> {
        let candidates = if self.explore.workspaces.is_empty() {
            super::ExploreStrand::discover_workspaces(&self.explore.workspace_root)
        } else {
            self.explore.workspaces.clone()
        };

        let healthy = candidates
            .into_iter()
            .filter(|workspace| {
                super::workspace_health::validate_workspace(workspace)
                    .quarantine_reason()
                    .is_none()
            })
            .collect::<Vec<_>>();
        let (mut targets, _duplicates) = super::workspace_health::resolve_duplicates(&healthy);
        targets.sort();

        if !targets.is_empty() {
            let mut hasher = DefaultHasher::new();
            self.qualified_id.hash(&mut hasher);
            let offset = (hasher.finish() as usize) % targets.len();
            targets.rotate_left(offset);
        }
        targets
    }
}

#[async_trait::async_trait]
impl super::Strand for FleetWeaveStrand {
    fn name(&self) -> &str {
        "weave"
    }

    fn is_generator(&self) -> bool {
        true
    }

    async fn evaluate(&self, _store: &dyn BeadStore, exclusions: &HashSet<BeadId>) -> StrandResult {
        if !self.config.enabled || !self.generation.enabled || !self.explore.enabled {
            return StrandResult::NoWork;
        }

        for workspace in self.targets() {
            let store = match crate::bead_store::discover_default(
                workspace.clone(),
                None,
                Some("needle".to_string()),
                Some(env!("CARGO_PKG_VERSION").to_string()),
            ) {
                Ok(store) => store,
                Err(error) => {
                    tracing::warn!(
                        workspace = %workspace.display(),
                        error = %error,
                        "workspace-aware Weave could not open target store"
                    );
                    continue;
                }
            };

            let target = WeaveStrand::new_shared(
                self.config.clone(),
                workspace,
                self.state_dir.clone(),
                self.agent.clone(),
                self.telemetry.clone(),
            )
            .with_generation(self.generation.clone(), self.exclude_labels.clone())
            .low_water_only();

            match target.evaluate(store.as_ref(), exclusions).await {
                // These results mean no model invocation occurred, so this
                // worker can safely try the next approved repository.
                StrandResult::Skipped { reason }
                    if reason == "generation_not_low_water"
                        || reason == "generation_lease_contended"
                        || reason == "no_home_store" =>
                {
                    continue;
                }
                // Every other result follows an acquired low-water lease. Stop
                // after it so one worker performs at most one creative pass per
                // cycle, even if the model reports NO_GAPS.
                result => return result,
            }
        }

        StrandResult::NoWork
    }
}

/// Default timeout for weave agent calls (60 seconds).
#[allow(dead_code)]
const WEAVE_AGENT_TIMEOUT_SECS: u64 = 60;

/// Maximum timeout for weave strand evaluation (300 seconds).
///
/// This prevents a single weave strand from stalling the entire SELECTING
/// cycle indefinitely while leaving enough time for a bounded production
/// prompt to complete under provider pressure. Production passes have finished
/// near the old 180-second boundary, so keep enough headroom to avoid discarding
/// an otherwise productive creator response. Store enumeration has its own
/// shorter timeout, so this remaining budget belongs to the creator agent.
/// See: needle-bf-5hlhn (weave strand stall investigation).
const WEAVE_STRAND_TIMEOUT_SECS: u64 = 300;

/// Production Weave invocation source.
enum WeaveInvocation {
    /// Compatibility path for callers that explicitly provide a CLI command.
    LegacyCli(String),
    /// The resolved NEEDLE adapter, including its invocation template, model,
    /// environment, and timeout policy.
    Adapter {
        adapter: Box<AgentAdapter>,
        global_timeout_secs: u64,
    },
    /// Deferred construction error. Strand construction is infallible, so an
    /// invalid runtime adapter becomes an observable creator failure.
    Unavailable(String),
}

/// Production `WeaveAgent` that invokes NEEDLE's resolved agent adapter.
pub struct CliWeaveAgent {
    invocation: WeaveInvocation,
}

impl CliWeaveAgent {
    /// Create a compatibility agent from an explicit executable command.
    /// Production waterfall construction uses [`Self::from_config`] instead.
    pub fn new(agent_cmd: String) -> Self {
        Self {
            invocation: WeaveInvocation::LegacyCli(agent_cmd),
        }
    }

    /// Resolve the configured adapter exactly as the ordinary dispatcher does.
    pub fn from_config(config: &Config) -> Result<Self> {
        let adapters = crate::dispatch::load_adapters(
            &config.agent.adapters_dir,
            &crate::dispatch::builtin_adapters(),
        )?;
        let adapter = adapters
            .get(&config.agent.default)
            .cloned()
            .with_context(|| {
                format!(
                    "configured Weave adapter '{}' was not found in {}",
                    config.agent.default,
                    config.agent.adapters_dir.display()
                )
            })?;
        Ok(Self {
            invocation: WeaveInvocation::Adapter {
                adapter: Box::new(adapter),
                global_timeout_secs: config.agent.timeout,
            },
        })
    }

    /// Preserve the waterfall's infallible construction while retaining the
    /// exact adapter resolution error for creator-failure telemetry.
    pub(crate) fn unavailable(error: impl Into<String>) -> Self {
        Self {
            invocation: WeaveInvocation::Unavailable(error.into()),
        }
    }
}

fn render_adapter_template(adapter: &AgentAdapter, workspace: &Path, prompt_file: &Path) -> String {
    adapter
        .invoke_template
        .replace("{workspace}", &workspace.display().to_string())
        .replace("{prompt_file}", &prompt_file.display().to_string())
        .replace("{bead_id}", "needle-weave")
        .replace("{model}", adapter.model.as_deref().unwrap_or("default"))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Extract the assistant's final text from common structured CLI streams.
/// Plain-text adapters pass through unchanged.
fn extract_agent_text(stdout: &str) -> Result<String> {
    if let Some(envelope) = crate::trace::parse_result_envelope(stdout) {
        if envelope.indicates_failure() {
            anyhow::bail!(
                "weave adapter reported terminal failure ({})",
                envelope.terminal_reason.as_deref().unwrap_or("is_error")
            );
        }
    }

    for line in stdout.lines().rev() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|value| value.as_str()) == Some("result") {
            if let Some(result) = value.get("result").and_then(|value| value.as_str()) {
                return Ok(result.to_string());
            }
        }
        if value.get("type").and_then(|value| value.as_str()) == Some("item.completed")
            && value
                .get("item")
                .and_then(|item| item.get("type"))
                .and_then(|value| value.as_str())
                == Some("agent_message")
        {
            if let Some(text) = value
                .get("item")
                .and_then(|item| item.get("text"))
                .and_then(|value| value.as_str())
            {
                return Ok(text.to_string());
            }
        }
    }

    Ok(stdout.to_string())
}

#[async_trait::async_trait]
impl WeaveAgent for CliWeaveAgent {
    async fn analyze_gaps(&self, prompt: &str, workspace: &Path) -> Result<String> {
        if let WeaveInvocation::Unavailable(error) = &self.invocation {
            anyhow::bail!("{error}");
        }

        // Write the prompt to a temp file.
        let tmp_dir = std::env::temp_dir().join("needle");
        std::fs::create_dir_all(&tmp_dir).context("failed to create needle temp dir for weave")?;
        let tmp_file = tmp_dir.join(format!(
            "weave-{}-{}.md",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(&tmp_file, prompt).context("failed to write weave prompt to temp file")?;

        let (cmd, child_env, stdin_from_prompt, invocation_name, timeout_secs) =
            match &self.invocation {
                WeaveInvocation::LegacyCli(agent_cmd) => (
                    format!(
                        "cd {} && {} --print < {}",
                        shell_quote(&workspace.display().to_string()),
                        agent_cmd,
                        shell_quote(&tmp_file.display().to_string()),
                    ),
                    std::collections::HashMap::new(),
                    false,
                    agent_cmd.clone(),
                    WEAVE_AGENT_TIMEOUT_SECS,
                ),
                WeaveInvocation::Adapter {
                    adapter,
                    global_timeout_secs,
                } => {
                    let timeout = if adapter.hard_timeout_secs > 0 {
                        adapter.hard_timeout_secs
                    } else {
                        adapter.effective_timeout(*global_timeout_secs).as_secs()
                    };
                    (
                        render_adapter_template(adapter, workspace, &tmp_file),
                        adapter.environment.clone(),
                        matches!(adapter.input_method, InputMethod::Stdin),
                        adapter.name.clone(),
                        timeout,
                    )
                }
                WeaveInvocation::Unavailable(_) => unreachable!("handled above"),
            };

        let prompt_stdin = if stdin_from_prompt {
            Some(
                std::fs::File::open(&tmp_file)
                    .context("failed to open weave prompt for adapter stdin")?,
            )
        } else {
            None
        };

        // Own process group (setpgid) so the kill guard below can target this
        // child and anything *it* forks (e.g. the CLI agent process, if the
        // shell doesn't exec-replace into it) without touching NEEDLE's own
        // process group. Spawn-then-wait (rather than the `.output()`
        // shorthand) so a PID is available to arm the guard before awaiting —
        // WeaveStrand::evaluate wraps this whole call in its own
        // WEAVE_STRAND_TIMEOUT_SECS timeout; if that fires while the agent is
        // still running, dropping this future must not silently orphan it.
        // See bf-653n7.
        let child_result = unsafe {
            let mut command = tokio::process::Command::new("bash");
            command
                .arg("-c")
                .arg(&cmd)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .envs(&child_env);
            if let Some(stdin) = prompt_stdin {
                command.stdin(std::process::Stdio::from(stdin));
            }
            command
                .pre_exec(|| {
                    libc::setpgid(0, 0);
                    Ok(())
                })
                .spawn()
                .with_context(|| format!("failed to spawn weave adapter: {invocation_name}"))
        };
        let child = match child_result {
            Ok(child) => child,
            Err(error) => {
                let _ = std::fs::remove_file(&tmp_file);
                return Err(error);
            }
        };
        let pid = child.id().unwrap_or(0);
        let mut kill_guard = ProcessGroupKillGuard::new(pid);

        let wait = child.wait_with_output();
        let output_result = if timeout_secs == 0 {
            wait.await
                .with_context(|| format!("failed to wait for weave adapter: {invocation_name}"))
        } else {
            match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), wait).await {
                Ok(result) => result.with_context(|| {
                    format!("failed to wait for weave adapter: {invocation_name}")
                }),
                Err(_) => Err(anyhow::anyhow!(
                    "weave adapter '{}' timed out after {}s",
                    invocation_name,
                    timeout_secs
                )),
            }
        };

        let output = match output_result {
            Ok(output) => output,
            Err(error) => {
                let _ = std::fs::remove_file(&tmp_file);
                return Err(error);
            }
        };
        kill_guard.disarm();

        // Always clean up the temp file.
        let _ = std::fs::remove_file(&tmp_file);

        if !output.status.success() {
            anyhow::bail!(
                "weave adapter '{}' exited with code {}",
                invocation_name,
                output.status.code().unwrap_or(-1)
            );
        }

        extract_agent_text(&String::from_utf8_lossy(&output.stdout))
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::{Filters, RepairReport};
    use crate::config::GenerationConfig;
    use crate::test_fixtures::fixture_root;
    use crate::types::{Bead, BeadId, BeadStatus, ClaimResult};

    use anyhow::Result;
    use chrono::{TimeZone, Utc};
    use std::sync::Mutex;

    // ── Mock WeaveAgent ──────────────────────────────────────────────────

    struct MockAgent {
        response: Mutex<String>,
    }

    impl MockAgent {
        fn new(response: &str) -> Self {
            MockAgent {
                response: Mutex::new(response.to_string()),
            }
        }
    }

    #[async_trait::async_trait]
    impl WeaveAgent for MockAgent {
        async fn analyze_gaps(&self, _prompt: &str, _workspace: &Path) -> Result<String> {
            Ok(self.response.lock().unwrap().clone())
        }
    }

    struct FailingAgent;

    #[async_trait::async_trait]
    impl WeaveAgent for FailingAgent {
        async fn analyze_gaps(&self, _prompt: &str, _workspace: &Path) -> Result<String> {
            anyhow::bail!("agent dispatch failed")
        }
    }

    // ── Mock BeadStore ───────────────────────────────────────────────────

    struct MockStore {
        beads: Vec<Bead>,
        created: Mutex<Vec<(String, String, Vec<String>)>>,
    }

    impl MockStore {
        fn new(beads: Vec<Bead>) -> Self {
            MockStore {
                beads,
                created: Mutex::new(Vec::new()),
            }
        }

        fn empty() -> Self {
            Self::new(vec![])
        }

        fn created_beads(&self) -> Vec<(String, String, Vec<String>)> {
            self.created.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for MockStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.beads.clone())
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(self.beads.clone())
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("not implemented")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn block(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
        async fn reopen(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
            self.created.lock().unwrap().push((
                title.to_string(),
                body.to_string(),
                labels.iter().map(|s| s.to_string()).collect(),
            ));
            Ok(BeadId::from(format!("weave-{}", title.len())))
        }
        async fn doctor_repair(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn doctor_check(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "claim_auto not supported in mock".to_string(),
            })
        }

        fn has_valid_store(&self) -> bool {
            true // Mock store always has a valid store
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn make_bead(id: &str, title: &str) -> Bead {
        let dt = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        Bead {
            id: BeadId::from(id.to_string()),
            title: title.to_string(),
            body: None,
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: vec![],
            workspace: fixture_root("test"),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: dt,
            updated_at: dt,
        }
    }

    fn make_test_workspace() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        // Create a documentation file.
        std::fs::write(
            workspace.join("README.md"),
            "# Test Project\n\nA test project.",
        )
        .unwrap();
        (dir, workspace)
    }

    fn make_enabled_config() -> WeaveConfig {
        WeaveConfig {
            enabled: true,
            max_beads_per_run: 5,
            cooldown_hours: 24,
            exclude_workspaces: vec![],
            doc_patterns: vec!["README*".to_string()],
            prompt_template: None,
        }
    }

    fn make_structural_workspace(root: &Path, name: &str, origin: &str) -> PathBuf {
        let workspace = root.join(name);
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();
        std::fs::create_dir_all(workspace.join(".git")).unwrap();
        std::fs::write(
            workspace.join(".git/config"),
            format!("[remote \"origin\"]\n  url = {origin}\n"),
        )
        .unwrap();
        workspace
    }

    use super::super::Strand;

    #[allow(dead_code)]
    fn test_telemetry() -> Telemetry {
        Telemetry::new("test".to_string())
    }

    // ── Tests ────────────────────────────────────────────────────────────

    #[test]
    fn strand_name_is_weave() {
        let dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());
        let strand = WeaveStrand::new(
            WeaveConfig::default(),
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Box::new(MockAgent::new("NO_GAPS")),
            telemetry,
        );
        assert_eq!(strand.name(), "weave");
    }

    #[tokio::test]
    async fn configured_adapter_uses_its_template_instead_of_its_name_as_a_binary() {
        let dir = tempfile::tempdir().unwrap();
        let adapters_dir = dir.path().join("adapters");
        std::fs::create_dir_all(&adapters_dir).unwrap();
        std::fs::write(
            adapters_dir.join("creator.yaml"),
            r#"name: creator-that-is-not-an-executable
agent_cli: definitely-not-a-real-binary
input_method:
  method: file
  path_template: "{prompt_file}"
invoke_template: "printf NO_GAPS"
timeout_secs: 5
"#,
        )
        .unwrap();

        let mut config = Config::default();
        config.agent.default = "creator-that-is-not-an-executable".to_string();
        config.agent.adapters_dir = adapters_dir;
        let agent = CliWeaveAgent::from_config(&config).unwrap();

        let response = agent.analyze_gaps("NO_GAPS", dir.path()).await.unwrap();
        assert_eq!(response, "NO_GAPS");
    }

    #[test]
    fn structured_agent_stream_yields_final_assistant_text() {
        let stream = concat!(
            "{\"type\":\"assistant\",\"message\":{}}\n",
            "{\"type\":\"result\",\"is_error\":false,\"result\":\"NO_GAPS\"}\n"
        );
        assert_eq!(extract_agent_text(stream).unwrap(), "NO_GAPS");
    }

    #[test]
    fn fleet_targets_match_explore_scope_and_exclude_invalid_homes() {
        let dir = tempfile::tempdir().unwrap();
        let first = make_structural_workspace(
            dir.path(),
            "first",
            "https://example.invalid/owner/first.git",
        );
        let second = make_structural_workspace(
            dir.path(),
            "second",
            "https://example.invalid/owner/second.git",
        );
        let virtual_home = dir.path().join("roam-only");
        std::fs::create_dir_all(&virtual_home).unwrap();

        let explore = ExploreConfig {
            workspaces: vec![first.clone(), virtual_home, second.clone()],
            ..Default::default()
        };
        let strand = FleetWeaveStrand::new(
            make_enabled_config(),
            explore,
            dir.path().join("state"),
            Box::new(MockAgent::new("NO_GAPS")),
            Telemetry::new("worker-a".to_string()),
            GenerationConfig::default(),
            Vec::new(),
            "worker-a".to_string(),
        );

        let mut actual = strand.targets();
        actual.sort();
        assert_eq!(actual, vec![first, second]);
    }

    #[tokio::test]
    async fn disabled_returns_no_work() {
        let dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());
        let strand = WeaveStrand::new(
            WeaveConfig::default(), // disabled by default
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Box::new(MockAgent::new("NO_GAPS")),
            telemetry,
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn excluded_workspace_returns_no_work() {
        let (_dir, workspace) = make_test_workspace();
        let state_dir = tempfile::tempdir().unwrap();
        let config = WeaveConfig {
            enabled: true,
            exclude_workspaces: vec![workspace.clone()],
            ..WeaveConfig::default()
        };
        let telemetry = Telemetry::new("test".to_string());
        let strand = WeaveStrand::new(
            config,
            workspace,
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new("NO_GAPS")),
            telemetry,
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn cooldown_not_elapsed_returns_no_work() {
        let (_dir, workspace) = make_test_workspace();
        let state_dir = tempfile::tempdir().unwrap();

        // Write state with recent last_run.
        let state = WeaveState {
            last_run: Some(Utc::now()),
            seen_titles: HashSet::new(),
        };
        let hash = workspace_hash(&workspace);
        let state_path = state_dir.path().join(format!("{hash}.json"));
        state.save(&state_path).unwrap();

        let strand = WeaveStrand::new(
            make_enabled_config(),
            workspace,
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new("NO_GAPS")),
            Telemetry::new("test".to_string()),
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn low_water_reserve_overrides_cooldown() {
        let (_dir, workspace) = make_test_workspace();
        let state_dir = tempfile::tempdir().unwrap();

        // Weave ran recently, so its ordinary cooldown is active.
        let state = WeaveState {
            last_run: Some(Utc::now()),
            seen_titles: HashSet::new(),
        };
        let hash = workspace_hash(&workspace);
        let state_path = state_dir.path().join(format!("{hash}.json"));
        state.save(&state_path).unwrap();

        // The fleet's eligible-ready frontier is empty, so it sits below the
        // reserve and the low-water gate lets this generator run anyway.
        let response = r#"[{"title": "Replenished gap", "body": "body", "priority": 2}]"#;
        let strand = WeaveStrand::new(
            make_enabled_config(),
            workspace,
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            Telemetry::new("worker-a".to_string()),
        )
        .with_generation(
            GenerationConfig {
                enabled: true,
                low_water_reserve: 1,
                lease_ttl_secs: 300,
            },
            Vec::new(),
        );

        let store = MockStore::empty();
        let result = strand.evaluate(&store, &HashSet::new()).await;

        assert!(
            matches!(result, StrandResult::WorkCreated),
            "low-water reserve should override the cooldown; got {:?}",
            result
        );
        assert_eq!(store.created_beads().len(), 1);
    }

    #[tokio::test]
    async fn fleet_target_does_not_generate_when_workspace_reserve_is_healthy() {
        let (_dir, workspace) = make_test_workspace();
        let state_dir = tempfile::tempdir().unwrap();
        let strand = WeaveStrand::new(
            make_enabled_config(),
            workspace,
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(
                r#"[{"title":"Should not exist","body":"body","priority":2}]"#,
            )),
            Telemetry::new("worker-a".to_string()),
        )
        .with_generation(
            GenerationConfig {
                enabled: true,
                low_water_reserve: 1,
                lease_ttl_secs: 300,
            },
            Vec::new(),
        )
        .low_water_only();

        let store = MockStore::new(vec![make_bead("ready", "Existing work")]);
        let result = strand.evaluate(&store, &HashSet::new()).await;

        assert!(matches!(
            result,
            StrandResult::Skipped { ref reason } if reason == "generation_not_low_water"
        ));
        assert!(store.created_beads().is_empty());
    }

    #[tokio::test]
    async fn contended_generation_lease_continues_without_generating() {
        let (_dir, workspace) = make_test_workspace();
        let state_dir = tempfile::tempdir().unwrap();

        let generation = GenerationConfig {
            enabled: true,
            low_water_reserve: 1,
            lease_ttl_secs: 300,
        };

        // The first empty worker sees the empty frontier, wins the
        // workspace-plus-strand lease, and fills the gap.
        let first_response = r#"[{"title": "Replenished gap", "body": "body", "priority": 2}]"#;
        let first = WeaveStrand::new(
            make_enabled_config(),
            workspace.clone(),
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(first_response)),
            Telemetry::new("worker-a".to_string()),
        )
        .with_generation(generation.clone(), Vec::new());
        let first_store = MockStore::empty();
        assert!(
            matches!(
                first.evaluate(&first_store, &HashSet::new()).await,
                StrandResult::WorkCreated
            ),
            "the first worker must generate while it holds the lease"
        );

        // A second empty worker sees the same empty frontier. The lease the
        // first worker took on this workspace-plus-strand pair is still live,
        // so this one must not ask the generator to fill the same gap.
        let second_response = r#"[{"title": "Duplicate gap", "body": "body", "priority": 2}]"#;
        let second = WeaveStrand::new(
            make_enabled_config(),
            workspace,
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(second_response)),
            Telemetry::new("worker-b".to_string()),
        )
        .with_generation(generation, Vec::new());

        let second_store = MockStore::empty();
        let result = second.evaluate(&second_store, &HashSet::new()).await;

        assert_eq!(
            second_store.created_beads().len(),
            0,
            "a contended lease must not generate a duplicate plan gap"
        );
        match result {
            StrandResult::Skipped { reason } => {
                assert_eq!(reason, "generation_lease_contended");
            }
            other => panic!("expected the waterfall to continue, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn no_docs_returns_no_work() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        // Empty workspace — no docs.
        let strand = WeaveStrand::new(
            make_enabled_config(),
            dir.path().to_path_buf(),
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new("NO_GAPS")),
            Telemetry::new("test".to_string()),
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn no_gaps_returns_no_work() {
        let (_dir, workspace) = make_test_workspace();
        let state_dir = tempfile::tempdir().unwrap();
        let strand = WeaveStrand::new(
            make_enabled_config(),
            workspace,
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new("NO_GAPS")),
            Telemetry::new("test".to_string()),
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn creates_beads_from_agent_response() {
        let (_dir, workspace) = make_test_workspace();
        let state_dir = tempfile::tempdir().unwrap();

        let response = r#"[
            {"title": "Add missing tests", "body": "Tests are missing for module X", "priority": 2},
            {"title": "Fix broken docs", "body": "The API docs reference deleted endpoints", "priority": 1}
        ]"#;

        let strand = WeaveStrand::new(
            make_enabled_config(),
            workspace,
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            Telemetry::new("test".to_string()),
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store, &HashSet::new()).await;

        assert!(
            matches!(result, StrandResult::WorkCreated),
            "should return WorkCreated; got {:?}",
            result
        );
        let created = store.created_beads();
        assert_eq!(created.len(), 2, "should create 2 beads");
        assert!(created[0].2.contains(&"weave-generated".to_string()));
        assert!(created[1].2.contains(&"weave-generated".to_string()));
    }

    #[tokio::test]
    async fn respects_max_beads_per_run() {
        let (_dir, workspace) = make_test_workspace();
        let state_dir = tempfile::tempdir().unwrap();

        let response = r#"[
            {"title": "Gap 1", "body": "body1", "priority": 1},
            {"title": "Gap 2", "body": "body2", "priority": 1},
            {"title": "Gap 3", "body": "body3", "priority": 1},
            {"title": "Gap 4", "body": "body4", "priority": 1}
        ]"#;

        let config = WeaveConfig {
            max_beads_per_run: 2,
            ..make_enabled_config()
        };
        let strand = WeaveStrand::new(
            config,
            workspace,
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            Telemetry::new("test".to_string()),
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store, &HashSet::new()).await;

        assert!(matches!(result, StrandResult::WorkCreated));
        assert_eq!(store.created_beads().len(), 2, "should only create 2 beads");
    }

    #[tokio::test]
    async fn dedup_skips_seen_titles() {
        let (_dir, workspace) = make_test_workspace();
        let state_dir = tempfile::tempdir().unwrap();

        // Pre-populate state with a seen title.
        let mut state = WeaveState::default();
        state.mark_seen("Gap 1");
        let hash = workspace_hash(&workspace);
        let state_path = state_dir.path().join(format!("{hash}.json"));
        state.save(&state_path).unwrap();

        // Set cooldown_hours to 0 so cooldown check passes.
        let response = r#"[
            {"title": "Gap 1", "body": "body1", "priority": 1},
            {"title": "Gap 2", "body": "body2", "priority": 1}
        ]"#;
        let config = WeaveConfig {
            cooldown_hours: 0,
            ..make_enabled_config()
        };
        let strand = WeaveStrand::new(
            config,
            workspace,
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            Telemetry::new("test".to_string()),
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store, &HashSet::new()).await;

        assert!(matches!(result, StrandResult::WorkCreated));
        let created = store.created_beads();
        assert_eq!(created.len(), 1, "should skip the seen title");
        assert_eq!(created[0].0, "Gap 2");
    }

    #[tokio::test]
    async fn dedup_skips_existing_bead_titles() {
        let (_dir, workspace) = make_test_workspace();
        let state_dir = tempfile::tempdir().unwrap();

        let response = r#"[
            {"title": "Existing Task", "body": "body", "priority": 1},
            {"title": "New Gap", "body": "body", "priority": 2}
        ]"#;

        let strand = WeaveStrand::new(
            make_enabled_config(),
            workspace,
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            Telemetry::new("test".to_string()),
        );
        let store = MockStore::new(vec![make_bead("bead-1", "existing task")]);
        let result = strand.evaluate(&store, &HashSet::new()).await;

        assert!(matches!(result, StrandResult::WorkCreated));
        let created = store.created_beads();
        assert_eq!(created.len(), 1, "should skip existing bead title");
        assert_eq!(created[0].0, "New Gap");
    }

    #[tokio::test]
    async fn agent_failure_returns_error() {
        let (_dir, workspace) = make_test_workspace();
        let state_dir = tempfile::tempdir().unwrap();
        let strand = WeaveStrand::new(
            make_enabled_config(),
            workspace,
            state_dir.path().to_path_buf(),
            Box::new(FailingAgent),
            Telemetry::new("test".to_string()),
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(
            matches!(result, StrandResult::Error(_)),
            "agent failure should return Error; got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn state_persisted_after_run() {
        let (_dir, workspace) = make_test_workspace();
        let state_dir = tempfile::tempdir().unwrap();

        let response = r#"[{"title": "New Gap", "body": "body", "priority": 1}]"#;
        let strand = WeaveStrand::new(
            make_enabled_config(),
            workspace.clone(),
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            Telemetry::new("test".to_string()),
        );
        let store = MockStore::empty();
        let _ = strand.evaluate(&store, &HashSet::new()).await;

        // Verify state was saved.
        let hash = workspace_hash(&workspace);
        let state_path = state_dir.path().join(format!("{hash}.json"));
        let state = WeaveState::load(&state_path).unwrap();
        assert!(state.last_run.is_some(), "last_run should be set");
        assert!(
            state.seen_titles.contains("new gap"),
            "created title should be tracked"
        );
    }

    #[tokio::test]
    async fn parses_json_in_code_fences() {
        let (_dir, workspace) = make_test_workspace();
        let state_dir = tempfile::tempdir().unwrap();

        let response = "Here are the gaps:\n```json\n[\n{\"title\": \"Fenced Gap\", \"body\": \"body\", \"priority\": 2}\n]\n```\n";
        let strand = WeaveStrand::new(
            make_enabled_config(),
            workspace,
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            Telemetry::new("test".to_string()),
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store, &HashSet::new()).await;

        assert!(matches!(result, StrandResult::WorkCreated));
        assert_eq!(store.created_beads()[0].0, "Fenced Gap");
    }

    // ── Parse tests ─────────────────────────────────────────────────────

    #[test]
    fn parse_no_gaps() {
        let result = WeaveStrand::parse_agent_response("NO_GAPS").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_json_array() {
        let input = r#"[{"title": "Fix X", "body": "Do Y", "priority": 1}]"#;
        let result = WeaveStrand::parse_agent_response(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Fix X");
        assert_eq!(result[0].priority, 1);
    }

    #[test]
    fn parse_json_object_with_beads_key() {
        let input = r#"{"beads": [{"title": "Fix X", "body": "Do Y", "priority": 2}]}"#;
        let result = WeaveStrand::parse_agent_response(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Fix X");
    }

    #[test]
    fn parse_fenced_json() {
        let input = "```json\n[{\"title\": \"Fix X\", \"body\": \"Do Y\", \"priority\": 3}]\n```";
        let result = WeaveStrand::parse_agent_response(input).unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_invalid_json_returns_error() {
        let result = WeaveStrand::parse_agent_response("not json at all");
        assert!(result.is_err());
    }

    // ── State tests ─────────────────────────────────────────────────────

    #[test]
    fn state_cooldown_elapsed_when_no_last_run() {
        let state = WeaveState::default();
        assert!(state.cooldown_elapsed(24));
    }

    #[test]
    fn state_cooldown_not_elapsed_when_recent() {
        let state = WeaveState {
            last_run: Some(Utc::now()),
            seen_titles: HashSet::new(),
        };
        assert!(!state.cooldown_elapsed(24));
    }

    #[test]
    fn state_cooldown_elapsed_when_old() {
        let state = WeaveState {
            last_run: Some(Utc::now() - chrono::Duration::hours(25)),
            seen_titles: HashSet::new(),
        };
        assert!(state.cooldown_elapsed(24));
    }

    #[test]
    fn state_dedup_case_insensitive() {
        let mut state = WeaveState::default();
        state.mark_seen("Fix Bug");
        assert!(state.is_duplicate("fix bug"));
        assert!(state.is_duplicate("FIX BUG"));
        assert!(!state.is_duplicate("Fix Different Bug"));
    }

    #[test]
    fn state_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let mut state = WeaveState {
            last_run: Some(Utc::now()),
            seen_titles: HashSet::new(),
        };
        state.mark_seen("title one");
        state.mark_seen("title two");
        state.save(&path).unwrap();

        let loaded = WeaveState::load(&path).unwrap();
        assert!(loaded.last_run.is_some());
        assert!(loaded.is_duplicate("title one"));
        assert!(loaded.is_duplicate("title two"));
    }

    #[test]
    fn state_load_missing_file_returns_default() {
        // A path inside a fresh fixture root that was never written.
        let path = fixture_root("missing-weave-state").join("state.json");
        let state = WeaveState::load(&path).unwrap();
        assert!(state.last_run.is_none());
        assert!(state.seen_titles.is_empty());
    }

    // ── Format tests ────────────────────────────────────────────────────

    #[test]
    fn format_existing_beads_empty() {
        assert_eq!(WeaveStrand::format_existing_beads(&[]), "(no open beads)");
    }

    #[test]
    fn format_existing_beads_list() {
        let beads = vec![make_bead("nd-1", "Fix the widget")];
        let result = WeaveStrand::format_existing_beads(&beads);
        assert!(result.contains("nd-1"));
        assert!(result.contains("Fix the widget"));
    }

    #[test]
    fn format_doc_files_empty() {
        let result = WeaveStrand::format_doc_files(&[], &fixture_root("ws-root"));
        assert_eq!(result, "(no documentation files found)");
    }

    #[test]
    fn format_doc_files_enforces_file_and_total_context_budgets() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for index in 0..6 {
            let path = dir.path().join(format!("doc-{index}.md"));
            std::fs::write(&path, "é".repeat(MAX_WEAVE_DOC_FILE_BYTES)).unwrap();
            files.push(path);
        }

        let result = WeaveStrand::format_doc_files(&files, dir.path());
        assert!(result.len() <= MAX_WEAVE_DOC_CONTEXT_BYTES);
        assert!(result.contains("[context truncated]"));
    }

    #[test]
    fn format_existing_beads_enforces_context_budget() {
        let beads = (0..2_000)
            .map(|index| {
                make_bead(
                    &format!("nd-{index}"),
                    &format!("Long planning title {index} {}", "x".repeat(100)),
                )
            })
            .collect::<Vec<_>>();

        let result = WeaveStrand::format_existing_beads(&beads);
        assert!(result.len() <= MAX_WEAVE_BEAD_CONTEXT_BYTES);
        assert!(result.contains("additional open beads omitted"));
    }

    // ── Workspace hash test ─────────────────────────────────────────────

    #[test]
    fn workspace_hash_is_deterministic() {
        let h1 = workspace_hash(Path::new("/home/user/project"));
        let h2 = workspace_hash(Path::new("/home/user/project"));
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn workspace_hash_differs_for_different_paths() {
        let h1 = workspace_hash(Path::new("/home/user/project-a"));
        let h2 = workspace_hash(Path::new("/home/user/project-b"));
        assert_ne!(h1, h2);
    }

    // ── Default config tests ────────────────────────────────────────────

    #[test]
    fn default_config_is_disabled() {
        let config = WeaveConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_beads_per_run, 5);
        assert_eq!(config.cooldown_hours, 24);
        assert!(config.exclude_workspaces.is_empty());
        assert!(!config.doc_patterns.is_empty());
    }
}
