//! Reflect strand: meta-analysis and learning consolidation.
//!
//! Strand 7 in the waterfall. Runs after all other strands return NoWork.
//! This is the last productive strategy before giving up — meta-analysis
//! only runs when there is genuinely nothing else to do.
//!
//! KAIROS-inspired four-phase cycle:
//! 1. **Orient** — read current learnings.md and existing skills, check sizes.
//! 2. **Gather** — read bead close bodies from issues.jsonl since last
//!    consolidation; read traces for failed beads.
//! 3. **Consolidate** — extract retrospective blocks, identify cross-bead
//!    patterns, merge into learnings.md, deduplicate, resolve contradictions.
//!    Promote learnings with reinforcement_count ≥ 3 to skill files.
//! 4. **Prune** — remove entries older than `learning_retention_days` days
//!    without reinforcement, compress similar entries, enforce `max_learnings`.
//!
//! Entry conditions (checked before running):
//! - `strands.reflect.enabled` is true
//! - 10+ beads closed since last consolidation (configurable)
//! - 24+ hours since last consolidation (configurable cooldown)
//!
//! Guardrails:
//! - Max `max_learnings_per_run` new learnings added per run
//! - Max `max_skills_per_run` skill files created/updated per run
//! - CLAUDE.md is read-only — never written
//!
//! Depends on: `config`, `learning`, `telemetry`, `types`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::bead_store::BeadStore;
use crate::claude_md_placement::observations_similar;
use crate::config::ReflectConfig;
use crate::drift::DriftDetector;
use crate::learning::{
    BeadType, Confidence, GlobalLearningsFile, LearningEntry, LearningsFile, Retrospective,
};
use crate::skill::{render_skill_file, SkillFrontmatter, SkillLibrary};
use crate::telemetry::{EventKind, Telemetry};
use crate::transcript::{ParsedTranscript, TranscriptDiscovery};
use crate::types::{BeadId, StrandError, StrandResult};

// ──────────────────────────────────────────────────────────────────────────────
// ReflectAgent trait
// ──────────────────────────────────────────────────────────────────────────────

/// Agent that infers a retrospective from an unstructured bead close body.
#[async_trait::async_trait]
#[allow(dead_code)]
pub trait ReflectAgent: Send + Sync {
    /// Extract a `Retrospective` from the given bead title and close body.
    /// Returns `Ok(None)` if no meaningful retrospective can be inferred.
    async fn extract_retrospective(
        &self,
        bead_title: &str,
        close_body: &str,
        workspace: &Path,
    ) -> Result<Option<Retrospective>>;
}

// ──────────────────────────────────────────────────────────────────────────────
// Default prompt template
// ──────────────────────────────────────────────────────────────────────────────

/// Default prompt template for retrospective extraction.
#[allow(dead_code)]
const DEFAULT_EXTRACTION_PROMPT: &str = r#"You are analyzing a completed software task.
Based on the title and close body below, write a concise retrospective section.

Title: {title}
Close body: {close_body}

Write ONLY the following markdown block — nothing before or after it:

## Retrospective
- **What worked:** <approaches that succeeded>
- **What didn't:** <approaches that failed or were harder than expected, or "N/A">
- **Surprise:** <anything unexpected about the codebase, tooling, or problem, or "N/A">
- **Reusable pattern:** <if this task type recurs, the key pattern to apply>
"#;

// ──────────────────────────────────────────────────────────────────────────────
// CliReflectAgent
// ──────────────────────────────────────────────────────────────────────────────

/// Production `ReflectAgent` that shells out to a CLI agent (e.g., `claude`).
///
/// The agent is invoked with the prompt fed via stdin redirection. The prompt
/// is built by substituting `{title}` and `{close_body}` in the template.
#[allow(dead_code)]
pub struct CliReflectAgent {
    /// Agent binary name or path (e.g., `"claude"`).
    agent_cmd: String,
    /// Optional custom prompt template. Uses `DEFAULT_EXTRACTION_PROMPT` if None.
    #[allow(dead_code)]
    prompt_template: Option<String>,
}

impl CliReflectAgent {
    /// Create a new `CliReflectAgent`.
    ///
    /// `agent_cmd` is the binary used for analysis (typically taken from
    /// `config.agent.default`). `prompt_template` is optional; if None, the
    /// built-in `DEFAULT_EXTRACTION_PROMPT` is used.
    #[allow(dead_code)]
    pub fn new(agent_cmd: String, prompt_template: Option<String>) -> Self {
        CliReflectAgent {
            agent_cmd,
            prompt_template,
        }
    }
}

#[async_trait::async_trait]
impl ReflectAgent for CliReflectAgent {
    async fn extract_retrospective(
        &self,
        bead_title: &str,
        close_body: &str,
        workspace: &Path,
    ) -> Result<Option<Retrospective>> {
        // Build the prompt by substituting template variables.
        let template = self
            .prompt_template
            .as_deref()
            .unwrap_or(DEFAULT_EXTRACTION_PROMPT);
        let prompt = template
            .replace("{title}", bead_title)
            .replace("{close_body}", close_body);

        // Write the prompt to a temp file.
        let tmp_path =
            std::path::PathBuf::from(format!("/tmp/needle-reflect-{}.md", std::process::id()));
        std::fs::write(&tmp_path, prompt)
            .with_context(|| format!("failed to write reflect prompt to {}", tmp_path.display()))?;

        // Run the agent: cd into workspace, pipe prompt to agent.
        let cmd = format!(
            "cd {} && {} < {}",
            workspace.display(),
            self.agent_cmd,
            tmp_path.display()
        );

        let output = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(&cmd)
            .output()
            .await
            .with_context(|| format!("failed to spawn reflect agent: {}", self.agent_cmd))?;

        // Always clean up the temp file.
        let _ = std::fs::remove_file(&tmp_path);

        if !output.status.success() {
            anyhow::bail!(
                "reflect agent exited with code {}",
                output.status.code().unwrap_or(-1)
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Retrospective::parse_from_close_body(&stdout)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// State persistence
// ──────────────────────────────────────────────────────────────────────────────

/// Persisted state for the Reflect strand.
#[derive(Debug, Serialize, Deserialize)]
struct ReflectState {
    /// Timestamp of the last successful consolidation.
    last_consolidation: DateTime<Utc>,
    /// Total closed beads at the time of last consolidation.
    beads_at_last_consolidation: u64,
}

impl ReflectState {
    fn load(state_dir: &Path) -> Result<Option<Self>> {
        let path = state_dir.join("reflect_state.json");
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read reflect state: {}", path.display()))?;
        let state: ReflectState =
            serde_json::from_str(&content).with_context(|| "failed to parse reflect state")?;
        Ok(Some(state))
    }

    fn save(&self, state_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("failed to create state dir: {}", state_dir.display()))?;
        let path = state_dir.join("reflect_state.json");
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)
            .with_context(|| format!("failed to write reflect state: {}", path.display()))?;
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Bead record (minimal, parsed from issues.jsonl)
// ──────────────────────────────────────────────────────────────────────────────

/// Minimal closed-bead record read from issues.jsonl.
#[derive(Debug, Deserialize)]
struct ClosedBeadRecord {
    id: String,
    title: Option<String>,
    status: String,
    close_reason: Option<String>,
    closed_at: Option<DateTime<Utc>>,
    assignee: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Summary
// ──────────────────────────────────────────────────────────────────────────────

/// Work performed during one Reflect cycle.
#[derive(Debug, Default)]
pub struct ReflectSummary {
    pub beads_processed: usize,
    pub learnings_added: usize,
    pub learnings_pruned: usize,
    pub skills_promoted: usize,
    /// Number of learnings promoted to the global learnings file.
    pub global_learnings_promoted: usize,
    /// Number of retrospectives extracted by the agent.
    pub agent_extractions: usize,
    /// Number of transcripts processed.
    pub transcripts_processed: usize,
    /// Number of learnings extracted from transcripts.
    pub transcript_learnings_added: usize,
    /// Number of learnings placed in CLAUDE.md files.
    pub claude_md_placed: usize,
    /// Number of drift clusters detected.
    pub drift_clusters_found: usize,
    /// Number of learnings extracted from drift clusters.
    pub drift_learnings_added: usize,
    /// Number of ADR decisions detected.
    pub adr_decisions_detected: usize,
}

// ──────────────────────────────────────────────────────────────────────────────
// Strand
// ──────────────────────────────────────────────────────────────────────────────

/// The Reflect strand — meta-analysis and learning consolidation.
pub struct ReflectStrand {
    config: ReflectConfig,
    workspace: PathBuf,
    state_dir: PathBuf,
    telemetry: Telemetry,
    /// Other workspaces to scan for cross-workspace patterns.
    known_workspaces: Vec<PathBuf>,
    /// Path to the global learnings file.
    global_learnings_path: PathBuf,
    /// Maximum entries in the global learnings file.
    max_global_learnings: usize,
    /// Optional agent for extracting retrospectives from close bodies.
    agent: Option<Box<dyn ReflectAgent>>,
}

impl ReflectStrand {
    /// Create a new ReflectStrand.
    ///
    /// - `config`: reflect strand configuration
    /// - `workspace`: workspace root (where `.beads/` lives)
    /// - `state_dir`: directory for persisting reflect state (`~/.needle/state/reflect/`)
    /// - `telemetry`: telemetry emitter
    /// - `agent`: optional agent for extracting retrospectives from close bodies
    pub fn new(
        config: ReflectConfig,
        workspace: PathBuf,
        state_dir: PathBuf,
        telemetry: Telemetry,
        agent: Option<Box<dyn ReflectAgent>>,
    ) -> Self {
        ReflectStrand {
            config,
            workspace,
            state_dir,
            telemetry,
            known_workspaces: Vec::new(),
            global_learnings_path: PathBuf::new(),
            max_global_learnings: 40,
            agent,
        }
    }

    /// Configure cross-workspace detection and global learnings promotion.
    ///
    /// - `known_workspaces`: other workspace paths to check for matching patterns
    /// - `global_learnings_path`: path to `~/.config/needle/global-learnings.md`
    /// - `max_global_learnings`: cap on total entries in the global file (default: 40)
    pub fn with_global(
        mut self,
        known_workspaces: Vec<PathBuf>,
        global_learnings_path: PathBuf,
        max_global_learnings: usize,
    ) -> Self {
        self.known_workspaces = known_workspaces;
        self.global_learnings_path = global_learnings_path;
        self.max_global_learnings = max_global_learnings;
        self
    }

    /// Run the four-phase consolidation cycle.
    ///
    /// `force` bypasses cooldown and minimum bead threshold checks (used by
    /// the `needle reflect` CLI command).
    pub async fn consolidate(&self, force: bool) -> Result<ReflectSummary> {
        // ── Phase 1: Orient ───────────────────────────────────────────────────
        let state = ReflectState::load(&self.state_dir)?;

        let issues_path = self.workspace.join(".beads").join("issues.jsonl");
        if !issues_path.exists() {
            tracing::debug!("reflect: no issues.jsonl found, skipping");
            return Ok(ReflectSummary::default());
        }

        // Count total closed beads and collect those since last consolidation.
        let all_closed = self.read_closed_beads(&issues_path)?;
        let total_closed = all_closed.len() as u64;

        let since_last: Vec<&ClosedBeadRecord> = match &state {
            Some(s) => all_closed
                .iter()
                .filter(|b| {
                    b.closed_at
                        .map(|t| t > s.last_consolidation)
                        .unwrap_or(false)
                })
                .collect(),
            None => all_closed.iter().collect(),
        };

        let beads_since = since_last.len();

        if !force {
            // Check minimum bead threshold.
            if beads_since < self.config.min_beads_since_last {
                tracing::debug!(
                    beads_since,
                    threshold = self.config.min_beads_since_last,
                    "reflect: below minimum bead threshold, skipping"
                );
                let _ = self.telemetry.emit(EventKind::ReflectSkipped {
                    reason: format!(
                        "only {} beads closed since last consolidation (need {})",
                        beads_since, self.config.min_beads_since_last
                    ),
                });
                return Ok(ReflectSummary::default());
            }

            // Check cooldown.
            if let Some(s) = &state {
                let hours_since = (Utc::now() - s.last_consolidation).num_hours() as u64;
                if hours_since < self.config.cooldown_hours {
                    tracing::debug!(
                        hours_since,
                        cooldown = self.config.cooldown_hours,
                        "reflect: cooldown not elapsed, skipping"
                    );
                    let _ = self.telemetry.emit(EventKind::ReflectSkipped {
                        reason: format!(
                            "cooldown not elapsed ({}/{}h)",
                            hours_since, self.config.cooldown_hours
                        ),
                    });
                    return Ok(ReflectSummary::default());
                }
            }
        }

        let _ = self.telemetry.emit(EventKind::ReflectStarted {
            beads_since_last: beads_since,
        });

        tracing::info!(beads_since, force, "reflect: starting consolidation cycle");

        // ── Phase 2: Gather ───────────────────────────────────────────────────
        let mut retro_entries: Vec<(String, Retrospective)> = Vec::new();
        let mut agent_extraction_count = 0usize;

        for bead in &since_last {
            if let Some(reason) = &bead.close_reason {
                match Retrospective::parse_from_close_body(reason) {
                    Ok(Some(retro)) if retro.is_meaningful() => {
                        retro_entries.push((bead.id.clone(), retro));
                    }
                    Ok(_) => {
                        // No explicit retrospective — try agent extraction
                        if let Some(agent) = &self.agent {
                            if agent_extraction_count < self.config.max_extraction_per_run {
                                let title = bead.title.as_deref().unwrap_or(&bead.id);
                                match agent
                                    .extract_retrospective(title, reason, &self.workspace)
                                    .await
                                {
                                    Ok(Some(retro)) if retro.is_meaningful() => {
                                        retro_entries.push((bead.id.clone(), retro));
                                        agent_extraction_count += 1;
                                    }
                                    Ok(_) => {}
                                    Err(e) => {
                                        tracing::warn!(
                                            bead_id = %bead.id,
                                            error = %e,
                                            "reflect: agent extraction failed"
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(bead_id = %bead.id, error = %e, "failed to parse retrospective");
                    }
                }
            }
        }

        tracing::debug!(
            retro_count = retro_entries.len(),
            agent_extraction_count,
            "reflect: gathered retrospectives"
        );

        // Gather transcripts and extract learning entries
        let (transcripts, transcript_entries) = self.extract_from_transcripts()
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "reflect: transcript extraction failed, continuing without transcripts");
                (Vec::new(), Vec::new())
            });

        let transcript_count = transcripts.len();

        tracing::debug!(
            transcript_count,
            transcript_entries = transcript_entries.len(),
            "reflect: gathered transcript entries"
        );

        // Detect drift across sessions
        let (drift_clusters_found, drift_entries) =
            self.detect_drift(&transcripts).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "reflect: drift detection failed, continuing");
                (0, Vec::new())
            });

        if drift_clusters_found > 0 {
            tracing::info!(
                drift_clusters_found,
                drift_learnings = drift_entries.len(),
                "reflect: detected drift across sessions"
            );
        }

        // Detect decision points for ADR records
        let adr_decisions_detected = self.detect_decisions(&transcripts).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "reflect: ADR decision detection failed, continuing");
            0
        });

        if adr_decisions_detected > 0 {
            tracing::info!(
                adr_decisions_detected,
                "reflect: detected ADR decision points"
            );
        }

        // ── Phase 3: Consolidate ──────────────────────────────────────────────
        let mut learnings = LearningsFile::load(&self.workspace)?;
        let mut summary = ReflectSummary {
            beads_processed: beads_since,
            ..Default::default()
        };

        // Extract learning entries from retrospectives.
        let mut candidate_entries: Vec<LearningEntry> = Vec::new();

        for (bead_id, retro) in &retro_entries {
            // Determine bead type from labels if available.
            let bead_type = self.infer_bead_type_from_id(bead_id, &all_closed);
            let worker = self.infer_worker_from_id(bead_id, &all_closed);

            if let Some(text) = &retro.reusable_pattern {
                candidate_entries.push(LearningEntry::new(
                    bead_id.clone(),
                    worker.clone(),
                    bead_type.clone(),
                    text.clone(),
                    Confidence::High,
                    format!("reusable-pattern from {bead_id}"),
                ));
            }
            if let Some(text) = &retro.what_worked {
                candidate_entries.push(LearningEntry::new(
                    bead_id.clone(),
                    worker.clone(),
                    bead_type.clone(),
                    text.clone(),
                    Confidence::Medium,
                    format!("what-worked from {bead_id}"),
                ));
            }
            if let Some(text) = &retro.surprise {
                candidate_entries.push(LearningEntry::new(
                    bead_id.clone(),
                    worker.clone(),
                    bead_type.clone(),
                    text.clone(),
                    Confidence::Medium,
                    format!("surprise from {bead_id}"),
                ));
            }
            if let Some(text) = &retro.what_didnt {
                candidate_entries.push(LearningEntry::new(
                    bead_id.clone(),
                    worker.clone(),
                    bead_type.clone(),
                    text.clone(),
                    Confidence::Low,
                    format!("what-didnt-work from {bead_id}"),
                ));
            }
        }

        // Add transcript-derived and drift-derived entries to candidates
        let transcript_entry_count = transcript_entries.len();
        let _drift_entry_count = drift_entries.len();
        let retro_entry_count = candidate_entries.len();
        candidate_entries.extend(transcript_entries);
        candidate_entries.extend(drift_entries);

        // Reinforce existing entries that match any closed bead ID, then add
        // non-duplicate candidates up to max_learnings_per_run.
        for bead in &since_last {
            let _ = learnings.reinforce_entry(&bead.id);
        }

        let mut added = 0usize;
        let mut transcript_added = 0usize;
        let mut drift_added = 0usize;
        for (idx, candidate) in candidate_entries.into_iter().enumerate() {
            if added >= self.config.max_learnings_per_run {
                break;
            }
            // Skip if a similar entry already exists (dedup).
            let similar = learnings.find_similar(&candidate.observation);
            if !similar.is_empty() {
                // Reinforce the most similar entry instead of adding a duplicate.
                let most_similar_id = similar[0].bead_id.clone();
                let _ = learnings.reinforce_entry(&most_similar_id);
                let _ = self.telemetry.emit(EventKind::ReflectLearningDeduplicated {
                    learning_id: candidate.bead_id.clone(),
                    existing_entry: most_similar_id,
                });
                continue;
            }
            learnings.add_entry(candidate)?;
            added += 1;
            // Track source: transcript entries come after retro entries,
            // drift entries come after transcript entries
            if idx >= retro_entry_count && idx < retro_entry_count + transcript_entry_count {
                transcript_added += 1;
            } else if idx >= retro_entry_count + transcript_entry_count {
                drift_added += 1;
            }
        }
        summary.learnings_added = added;
        summary.transcripts_processed = transcript_count;
        summary.transcript_learnings_added = transcript_added;
        summary.drift_learnings_added = drift_added;

        // Promote high-reinforcement entries to skill files.
        let promoted = self.promote_to_skills(learnings.entries())?;
        summary.skills_promoted = promoted;

        // Promote cross-workspace patterns to global learnings.
        let global_promoted = self.promote_cross_workspace_patterns(&learnings)?;
        summary.global_learnings_promoted = global_promoted;

        // Promote learnings to CLAUDE.md files at appropriate ancestor levels.
        let claude_md_promoted = self.promote_to_claude_md(&learnings)?;
        summary.claude_md_placed = claude_md_promoted;
        summary.agent_extractions = agent_extraction_count;
        summary.drift_clusters_found = drift_clusters_found;
        summary.adr_decisions_detected = adr_decisions_detected;

        // ── Phase 4: Prune ────────────────────────────────────────────────────
        let pruned = learnings.prune_stale()?;
        summary.learnings_pruned = pruned;

        // Enforce max_learnings cap.
        let over_limit = learnings.consolidate(self.config.max_learnings)?;
        summary.learnings_pruned += over_limit;

        // Persist updated state.
        let new_state = ReflectState {
            last_consolidation: Utc::now(),
            beads_at_last_consolidation: total_closed,
        };
        new_state.save(&self.state_dir)?;

        let _ = self.telemetry.emit(EventKind::ReflectConsolidated {
            learnings_added: summary.learnings_added,
            learnings_pruned: summary.learnings_pruned,
            skills_promoted: summary.skills_promoted,
            beads_processed: summary.beads_processed,
        });

        tracing::info!(
            learnings_added = summary.learnings_added,
            learnings_pruned = summary.learnings_pruned,
            skills_promoted = summary.skills_promoted,
            beads_processed = summary.beads_processed,
            "reflect: consolidation complete"
        );

        Ok(summary)
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Read all closed beads from issues.jsonl.
    fn read_closed_beads(&self, issues_path: &Path) -> Result<Vec<ClosedBeadRecord>> {
        let content = std::fs::read_to_string(issues_path)
            .with_context(|| format!("failed to read {}", issues_path.display()))?;

        let mut beads = Vec::new();
        for (line_no, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<ClosedBeadRecord>(line) {
                Ok(record) if record.status == "closed" => beads.push(record),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(line = line_no + 1, error = %e, "reflect: failed to parse issues.jsonl line");
                }
            }
        }
        Ok(beads)
    }

    /// Infer bead type from labels in the closed bead record.
    fn infer_bead_type_from_id(&self, bead_id: &str, all_closed: &[ClosedBeadRecord]) -> BeadType {
        let record = all_closed.iter().find(|b| b.id == bead_id);
        if let Some(rec) = record {
            for label in &rec.labels {
                if let Some(bt) = BeadType::from_str(label) {
                    return bt;
                }
            }
        }
        BeadType::Other
    }

    /// Infer worker from the assignee field of the closed bead record.
    fn infer_worker_from_id(&self, bead_id: &str, all_closed: &[ClosedBeadRecord]) -> String {
        all_closed
            .iter()
            .find(|b| b.id == bead_id)
            .and_then(|b| b.assignee.clone())
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Promote learning entries with reinforcement_count >= 3 to skill files.
    ///
    /// Creates `.beads/skills/<slug>.md` for each promoted entry.
    /// Returns the number of skills created this run.
    fn promote_to_skills(&self, entries: &[LearningEntry]) -> Result<usize> {
        let skills_dir = self.workspace.join(".beads").join("skills");
        let mut promoted = 0usize;

        // Build set of already-promoted bead IDs to avoid re-promoting.
        let already_promoted = self.read_promoted_ids(&skills_dir);

        for entry in entries {
            if promoted >= self.config.max_skills_per_run {
                break;
            }
            if entry.reinforcement_count < 3 {
                continue;
            }
            if already_promoted.contains(&entry.bead_id) {
                continue;
            }

            match self.write_skill_file(&skills_dir, entry) {
                Ok(()) => promoted += 1,
                Err(e) => {
                    tracing::warn!(bead_id = %entry.bead_id, error = %e, "reflect: failed to write skill file");
                }
            }
        }

        Ok(promoted)
    }

    /// Read the set of bead IDs that have already been promoted to skills.
    fn read_promoted_ids(&self, _skills_dir: &Path) -> std::collections::HashSet<String> {
        match SkillLibrary::load(&self.workspace) {
            Ok(lib) => lib.promoted_source_beads(),
            Err(e) => {
                tracing::warn!(error = %e, "reflect: failed to load skill library for promoted IDs");
                std::collections::HashSet::new()
            }
        }
    }

    /// Scan known workspaces for learnings that match entries in the current workspace,
    /// promoting matches (cross-workspace patterns) to the global learnings file.
    ///
    /// An entry is promoted when a similar observation appears in at least one other
    /// known workspace. Returns the number of entries promoted this run.
    fn promote_cross_workspace_patterns(&self, learnings: &LearningsFile) -> Result<usize> {
        // Skip when not configured.
        if self.global_learnings_path.as_os_str().is_empty() || self.known_workspaces.is_empty() {
            return Ok(0);
        }

        let mut global = GlobalLearningsFile::load(&self.global_learnings_path)?;

        // Load learnings files for all known workspaces, skipping failures.
        let mut other_learnings: Vec<LearningsFile> = Vec::new();
        for ws in &self.known_workspaces {
            match LearningsFile::load(ws) {
                Ok(lf) if !lf.entries().is_empty() => other_learnings.push(lf),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        workspace = %ws.display(),
                        error = %e,
                        "reflect: failed to load learnings for cross-workspace scan"
                    );
                }
            }
        }

        if other_learnings.is_empty() {
            return Ok(0);
        }

        let mut promoted = 0usize;

        for entry in learnings.entries() {
            // Check if any other workspace has a similar observation.
            let appears_in_other = other_learnings
                .iter()
                .any(|other| !other.find_similar(&entry.observation).is_empty());

            if !appears_in_other {
                continue;
            }

            if global.promote(entry.clone(), self.max_global_learnings) {
                promoted += 1;
                let _ = self.telemetry.emit(EventKind::ReflectLearningPromoted {
                    learning_id: entry.bead_id.clone(),
                    target_path: self.global_learnings_path.display().to_string(),
                    workspace_count: self.known_workspaces.len() + 1,
                    is_decision: entry.decision_context.is_some(),
                });
                tracing::info!(
                    bead_id = %entry.bead_id,
                    observation = %entry.observation,
                    "reflect: promoted cross-workspace learning to global"
                );
            }
        }

        if promoted > 0 {
            global.write()?;
        }

        Ok(promoted)
    }

    /// Promote high-reinforcement learnings to CLAUDE.md files using the lowest common ancestor strategy.
    ///
    /// When a learning appears across multiple workspaces, it's written to the CLAUDE.md
    /// at the lowest common ancestor directory covering all contributing workspaces.
    /// Single-workspace learnings go to that workspace's CLAUDE.md.
    fn promote_to_claude_md(&self, learnings: &LearningsFile) -> Result<usize> {
        use crate::claude_md_placement::{ClaudeMdPlacer, PromotedLearning};

        // Skip when not configured
        if !self.config.claude_md_placement {
            return Ok(0);
        }

        // Skip if no known workspaces configured
        if self.known_workspaces.is_empty() {
            return Ok(0);
        }

        // Build workspace list including current workspace
        let mut all_workspaces = vec![self.workspace.clone()];
        all_workspaces.extend(self.known_workspaces.clone());

        // Create placer with all known workspaces
        let placer = ClaudeMdPlacer::new(all_workspaces.clone());

        // Load learnings from all workspaces to find cross-workspace patterns
        let mut workspace_learnings: std::collections::HashMap<PathBuf, Vec<LearningEntry>> =
            std::collections::HashMap::new();

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
                        "reflect: failed to load learnings for CLAUDE.md placement"
                    );
                }
            }
        }

        let mut placed = 0usize;

        // For each entry in current workspace, check if it appears in other workspaces
        for entry in learnings.entries() {
            // Only promote high-confidence or high-reinforcement entries
            if entry.confidence != crate::learning::Confidence::High
                && entry.reinforcement_count < 2
            {
                continue;
            }

            let mut matching_workspaces = vec![self.workspace.clone()];

            for (ws, entries) in &workspace_learnings {
                if ws == &self.workspace {
                    continue;
                }

                // Check if any entry in this workspace is similar
                let has_similar = entries
                    .iter()
                    .any(|e| observations_similar(&e.observation, &entry.observation));

                if has_similar {
                    matching_workspaces.push(ws.clone());
                }
            }

            // Place the learning at the appropriate CLAUDE.md
            let promoted = PromotedLearning::new(entry.clone(), matching_workspaces);
            match placer.place_learning(&promoted) {
                Ok(true) => {
                    placed += 1;
                    // The placer doesn't return the exact path, so we use the workspace info
                    let target_path = if promoted.source_workspaces.len() > 1 {
                        // Cross-workspace learning - would be placed at LCA
                        format!("LCA of {} workspaces", promoted.source_workspaces.len())
                    } else {
                        self.workspace.join("CLAUDE.md").display().to_string()
                    };
                    let _ = self.telemetry.emit(EventKind::ReflectClaudeMdWritten {
                        path: target_path,
                        entries_added: 1,
                        entries_updated: 0,
                    });
                    tracing::info!(
                        bead_id = %entry.bead_id,
                        observation = %entry.observation,
                        workspaces = ?promoted.source_workspaces,
                        "reflect: placed learning in CLAUDE.md"
                    );
                }
                Ok(false) => {
                    // Duplicate, skipped
                }
                Err(e) => {
                    tracing::warn!(
                        bead_id = %entry.bead_id,
                        error = %e,
                        "reflect: failed to place learning in CLAUDE.md"
                    );
                }
            }
        }

        Ok(placed)
    }

    /// Simple similarity check for observation text (shared with claude_md_placement).
    fn observations_similar(a: &str, b: &str) -> bool {
        use std::collections::HashSet;
        let a_lower = a.to_lowercase();
        let b_lower = b.to_lowercase();

        let a_words: HashSet<&str> = a_lower.split_whitespace().collect();
        let b_words: HashSet<&str> = b_lower.split_whitespace().collect();

        let shared = a_words.intersection(&b_words).count();
        let min_len = a_words.len().min(b_words.len());

        shared >= 2 && (shared as f32) >= (min_len as f32 * 0.5)
            || a_lower.contains(&b_lower)
            || b_lower.contains(&a_lower)
    }

    /// Write a skill file for a promoted learning entry (YAML frontmatter format).
    fn write_skill_file(&self, skills_dir: &Path, entry: &LearningEntry) -> Result<()> {
        std::fs::create_dir_all(skills_dir)
            .with_context(|| format!("failed to create skills dir: {}", skills_dir.display()))?;

        let slug = slugify(&entry.observation);
        let filename = format!("{}-{}.md", &slug[..slug.len().min(40)], &entry.bead_id);
        let path = skills_dir.join(&filename);

        let frontmatter = SkillFrontmatter {
            task_types: vec![entry.bead_type.as_str().to_string()],
            labels: vec![],
            success_count: 0,
            last_used: None,
            source_beads: vec![entry.bead_id.clone()],
        };

        let body = format!(
            "## {}\n\n{}\n\n**Worker:** {}\n**Source:** {}\n",
            truncate(&entry.observation, 60),
            entry.observation,
            entry.worker,
            entry.source,
        );

        let content = render_skill_file(&frontmatter, &body)
            .with_context(|| format!("failed to render skill file: {filename}"))?;

        std::fs::write(&path, content)
            .with_context(|| format!("failed to write skill file: {}", path.display()))?;

        tracing::info!(
            bead_id = %entry.bead_id,
            file = %filename,
            reinforcement_count = entry.reinforcement_count,
            "reflect: promoted learning to skill file"
        );
        Ok(())
    }

    /// Discover and parse recent transcripts, extract learning entries from them.
    ///
    /// Returns a tuple of (transcripts, learning_entries).
    fn extract_from_transcripts(&self) -> Result<(Vec<ParsedTranscript>, Vec<LearningEntry>)> {
        use std::collections::HashMap;

        // Create transcript discovery with recency cutoff
        let recency_cutoff =
            Utc::now() - Duration::days(self.config.transcript_recency_days as i64);
        let discovery = TranscriptDiscovery::new(
            &self.workspace,
            None, // Use default ~/.claude
            self.config.transcript_max_sessions,
        )
        .with_recency_cutoff(recency_cutoff);

        // Discover and parse transcripts
        let transcripts = discovery
            .discover()
            .with_context(|| "failed to discover transcripts")?;

        if transcripts.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        tracing::debug!(
            count = transcripts.len(),
            "reflect: discovered recent transcripts"
        );

        let _ = self.telemetry.emit(EventKind::ReflectTranscriptsRead {
            sessions_count: transcripts.len(),
            entries_count: transcripts.iter().map(|t| t.actions.len()).sum(),
            parse_errors: 0, // TODO: track parse errors if discovery reports them
        });

        let mut entries: Vec<LearningEntry> = Vec::new();
        let mut tool_usage_counts: HashMap<String, usize> = HashMap::new();

        for transcript in &transcripts {
            // Count tool usage patterns
            for action in &transcript.actions {
                if let Some(ref tool_name) = action.tool_name {
                    *tool_usage_counts.entry(tool_name.clone()).or_insert(0) += 1;
                }
            }

            // Extract bead ID if present
            let bead_id = transcript
                .bead_id
                .clone()
                .unwrap_or_else(|| BeadId::from(format!("session-{}", transcript.session_id)));
            let bead_id_str = bead_id.to_string();

            // Infer worker from context or use "needle"
            let worker = "needle".to_string();

            // Detect decisions in this transcript (ADR-style extraction)
            let decisions = crate::transcript::detect_decisions(transcript);
            for decision in decisions {
                let observation = format!("Decision: {}", truncate(&decision.decision, 200));

                // Convert detected decision to learning entry with ADR context
                let decision_context: crate::learning::DecisionContext = decision.clone().into();
                let entry = LearningEntry::with_adr_context(
                    bead_id_str.clone(),
                    worker.clone(),
                    BeadType::Feature, // Decisions often relate to feature choices
                    observation,
                    Confidence::Medium, // Decisions get medium confidence by default
                    format!("transcript decision: {}", transcript.session_id),
                    decision_context,
                );
                entries.push(entry);

                let _ = self.telemetry.emit(EventKind::ReflectDecisionExtracted {
                    bead_id: bead_id.clone(),
                    has_alternatives: !decision.alternatives.is_empty(),
                    rationale_length: decision.rationale.len(),
                });

                tracing::debug!(
                    decision = %decision.decision,
                    confidence = decision.confidence,
                    "reflect: extracted decision from transcript"
                );
            }

            // Extract learning entries from transcript actions (non-decision patterns)
            for action in &transcript.actions {
                let entry = match action.action_type {
                    crate::transcript::ActionType::ToolUse => {
                        // For tool use, create an entry about the tool usage pattern
                        if let Some(ref tool_name) = action.tool_name {
                            let observation = format!(
                                "Tool usage pattern: {} — {}",
                                tool_name, action.description
                            );
                            Some(LearningEntry::new(
                                bead_id_str.clone(),
                                worker.clone(),
                                BeadType::Other,
                                observation,
                                Confidence::Medium,
                                format!("transcript: {}", transcript.session_id),
                            ))
                        } else {
                            None
                        }
                    }
                    crate::transcript::ActionType::Text => {
                        // For text actions, look for patterns in the output
                        if action.description.len() > 50 {
                            // Only extract longer text responses as patterns
                            let observation = format!(
                                "Assistant response pattern: {}",
                                truncate(&action.description, 200)
                            );
                            Some(LearningEntry::new(
                                bead_id_str.clone(),
                                worker.clone(),
                                BeadType::Other,
                                observation,
                                Confidence::Low,
                                format!("transcript: {}", transcript.session_id),
                            ))
                        } else {
                            None
                        }
                    }
                    crate::transcript::ActionType::Thinking => {
                        // Thinking blocks may contain useful reasoning patterns
                        // Note: decisions are already extracted above, so skip decision-heavy blocks
                        if action.description.len() > 30 {
                            let observation = format!(
                                "Reasoning pattern: {}",
                                truncate(&action.description, 200)
                            );
                            Some(LearningEntry::new(
                                bead_id_str.clone(),
                                worker.clone(),
                                BeadType::Other,
                                observation,
                                Confidence::Low,
                                format!("transcript: {}", transcript.session_id),
                            ))
                        } else {
                            None
                        }
                    }
                };

                if let Some(entry) = entry {
                    entries.push(entry);
                }
            }
        }

        // Log tool usage summary
        if !tool_usage_counts.is_empty() {
            let mut summary: Vec<_> = tool_usage_counts.iter().collect();
            summary.sort_by(|a, b| b.1.cmp(a.1));
            tracing::debug!(
                top_tools = ?summary.iter().take(5).map(|(k, v)| (k, v)).collect::<Vec<_>>(),
                "reflect: transcript tool usage summary"
            );
        }

        Ok((transcripts, entries))
    }

    /// Detect drift across sessions and write a drift report.
    ///
    /// Returns the number of drift clusters found and the learning entries
    /// extracted from drift patterns for feeding into the consolidation pipeline.
    fn detect_drift(
        &self,
        transcripts: &[ParsedTranscript],
    ) -> Result<(usize, Vec<LearningEntry>)> {
        if !self.config.drift_enabled {
            let _ = self.telemetry.emit(EventKind::DriftDetectionSkipped {
                reason: "drift detection disabled in config".to_string(),
            });
            return Ok((0, Vec::new()));
        }

        if transcripts.len() < 2 {
            let _ = self.telemetry.emit(EventKind::DriftDetectionSkipped {
                reason: format!("need at least 2 sessions, got {}", transcripts.len()),
            });
            return Ok((0, Vec::new()));
        }

        let _ = self.telemetry.emit(EventKind::DriftDetectionStarted {
            sessions_analyzed: transcripts.len(),
        });

        // Create drift detector with configured similarity threshold
        let detector = DriftDetector::new(self.config.drift_similarity_threshold);

        // Detect drift
        let report = detector.detect(transcripts)?;

        // Categorize clusters
        let evolved_count = report
            .clusters
            .iter()
            .filter(|c| c.category == crate::drift::DriftCategory::Evolved)
            .count();
        let inconsistent_count = report
            .clusters
            .iter()
            .filter(|c| c.category == crate::drift::DriftCategory::Inconsistent)
            .count();

        let _ = self.telemetry.emit(EventKind::DriftDetectionCompleted {
            sessions_analyzed: report.sessions_analyzed,
            clusters_found: report.clusters_detected,
            evolved_count,
            inconsistent_count,
        });

        // Emit ReflectDriftDetected for each cluster
        for cluster in &report.clusters {
            let sessions: Vec<String> = cluster
                .sessions
                .iter()
                .map(|s| s.session_id.clone())
                .collect();
            let _ = self.telemetry.emit(EventKind::ReflectDriftDetected {
                cluster_size: cluster.sessions.len(),
                category: cluster.category.as_str().to_string(),
                sessions,
            });
        }

        // Extract learning entries from drift clusters
        let drift_entries = report.to_learning_entries();

        // Emit ReflectDriftPromoted for each drift cluster that produced learning entries
        for cluster in &report.clusters {
            let pattern = match cluster.category {
                crate::drift::DriftCategory::Evolved => "evolved-solution-pattern".to_string(),
                crate::drift::DriftCategory::Inconsistent => {
                    "inconsistent-approach-pattern".to_string()
                }
                crate::drift::DriftCategory::Unknown => "divergent-approach-pattern".to_string(),
            };
            let _ = self.telemetry.emit(EventKind::ReflectDriftPromoted {
                pattern: pattern.clone(),
                category: cluster.category.as_str().to_string(),
            });
        }

        // Write drift report if any clusters were found
        if report.clusters_detected > 0 {
            let drifts_dir = self.workspace.join(".beads").join("drifts");
            let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
            let report_path = drifts_dir.join(format!("drift-{}.md", timestamp));

            report.write(&report_path)?;

            let _ = self.telemetry.emit(EventKind::DriftReportWritten {
                report_path: report_path.display().to_string(),
                clusters: report.clusters_detected,
            });

            tracing::info!(
                report_path = %report_path.display(),
                clusters = report.clusters_detected,
                evolved = evolved_count,
                inconsistent = inconsistent_count,
                drift_learnings = drift_entries.len(),
                "reflect: drift detection complete"
            );
        }

        Ok((report.clusters_detected, drift_entries))
    }

    /// Detect decision points in transcripts and write ADR records.
    ///
    /// Returns the number of decisions detected.
    fn detect_decisions(&self, transcripts: &[ParsedTranscript]) -> Result<usize> {
        if !self.config.adr_enabled {
            let _ = self.telemetry.emit(EventKind::DecisionDetectionSkipped {
                reason: "ADR decision extraction disabled in config".to_string(),
            });
            return Ok(0);
        }

        if transcripts.is_empty() {
            let _ = self.telemetry.emit(EventKind::DecisionDetectionSkipped {
                reason: "no transcripts to analyze".to_string(),
            });
            return Ok(0);
        }

        let _ = self.telemetry.emit(EventKind::DecisionDetectionStarted {
            sessions_analyzed: transcripts.len(),
        });

        // Create decision detector
        let detector = crate::decision::DecisionDetector::new();

        // Detect decisions
        let analysis = detector.analyze(transcripts)?;

        let _ = self.telemetry.emit(EventKind::DecisionDetectionCompleted {
            sessions_analyzed: analysis.transcripts_analyzed,
            decisions_found: analysis.decisions.len(),
        });

        // Write ADR records if any decisions were found
        if !analysis.decisions.is_empty() {
            let decisions_dir = self.workspace.join(".beads").join("decisions");
            std::fs::create_dir_all(&decisions_dir).with_context(|| {
                format!(
                    "failed to create decisions dir: {}",
                    decisions_dir.display()
                )
            })?;

            for decision in &analysis.decisions {
                let path = decisions_dir.join(format!("{}.md", decision.id));
                let content = decision.to_adr_markdown();
                std::fs::write(&path, content)
                    .with_context(|| format!("failed to write ADR: {}", path.display()))?;

                let bead_id = decision
                    .bead_id
                    .clone()
                    .unwrap_or_else(|| BeadId::from(format!("session-{}", decision.session_id)));
                let _ = self.telemetry.emit(EventKind::ReflectAdrCreated {
                    bead_id,
                    path: path.display().to_string(),
                });

                tracing::debug!(
                    decision_id = %decision.id,
                    title = %decision.title,
                    "reflect: wrote ADR record"
                );
            }

            tracing::info!(
                decisions_count = analysis.decisions.len(),
                "reflect: ADR decision detection complete"
            );
        }

        Ok(analysis.decisions.len())
    }
}

#[async_trait::async_trait]
impl super::Strand for ReflectStrand {
    fn name(&self) -> &str {
        "reflect"
    }

    async fn evaluate(&self, _store: &dyn BeadStore) -> StrandResult {
        if !self.config.enabled {
            return StrandResult::NoWork;
        }

        match self.consolidate(false).await {
            Ok(_) => StrandResult::NoWork,
            Err(e) => StrandResult::Error(StrandError::ConfigError(format!(
                "reflect consolidation failed: {e}"
            ))),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Utilities
// ──────────────────────────────────────────────────────────────────────────────

/// Create a URL-safe slug from a string.
fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Truncate a string to at most `max_chars` characters.
fn truncate(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        s
    } else {
        &s[..max_chars]
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strand::Strand as _;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("foo_bar baz"), "foo-bar-baz");
        assert_eq!(slugify("multiple---dashes"), "multiple-dashes");
    }

    #[test]
    fn truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long() {
        assert_eq!(truncate("hello world", 5), "hello");
    }

    #[test]
    fn reflect_agent_default_prompt_contains_fields() {
        assert!(DEFAULT_EXTRACTION_PROMPT.contains("What worked"));
        assert!(DEFAULT_EXTRACTION_PROMPT.contains("What didn't"));
        assert!(DEFAULT_EXTRACTION_PROMPT.contains("Surprise"));
        assert!(DEFAULT_EXTRACTION_PROMPT.contains("Reusable pattern"));
    }

    #[test]
    fn cli_reflect_agent_new_stores_cmd() {
        let agent = CliReflectAgent::new("claude".to_string(), None);
        assert_eq!(agent.agent_cmd, "claude");
    }

    #[test]
    fn reflect_strand_name() {
        let config = ReflectConfig::default();
        let tel = Telemetry::new("test".to_string());
        let strand = ReflectStrand::new(
            config,
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/state"),
            tel,
            None,
        );
        assert_eq!(strand.name(), "reflect");
    }

    #[tokio::test]
    async fn reflect_disabled_skips() {
        struct NoOpStore;
        #[async_trait::async_trait]
        impl crate::bead_store::BeadStore for NoOpStore {
            async fn list_all(&self) -> anyhow::Result<Vec<crate::types::Bead>> {
                Ok(vec![])
            }
            async fn ready(
                &self,
                _f: &crate::bead_store::Filters,
            ) -> anyhow::Result<Vec<crate::types::Bead>> {
                Ok(vec![])
            }
            async fn show(&self, _id: &crate::types::BeadId) -> anyhow::Result<crate::types::Bead> {
                anyhow::bail!("not found")
            }
            async fn claim(
                &self,
                _id: &crate::types::BeadId,
                _a: &str,
            ) -> anyhow::Result<crate::types::ClaimResult> {
                anyhow::bail!("not impl")
            }
            async fn release(&self, _id: &crate::types::BeadId) -> anyhow::Result<()> {
                Ok(())
            }
            async fn flush(&self) -> anyhow::Result<()> {
                Ok(())
            }
            async fn reopen(&self, _id: &crate::types::BeadId) -> anyhow::Result<()> {
                Ok(())
            }
            async fn labels(&self, _id: &crate::types::BeadId) -> anyhow::Result<Vec<String>> {
                Ok(vec![])
            }
            async fn add_label(&self, _id: &crate::types::BeadId, _l: &str) -> anyhow::Result<()> {
                Ok(())
            }
            async fn remove_label(
                &self,
                _id: &crate::types::BeadId,
                _l: &str,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn create_bead(
                &self,
                _t: &str,
                _b: &str,
                _l: &[&str],
            ) -> anyhow::Result<crate::types::BeadId> {
                Ok(crate::types::BeadId::from("new".to_string()))
            }
            async fn doctor_repair(&self) -> anyhow::Result<crate::bead_store::RepairReport> {
                Ok(crate::bead_store::RepairReport::default())
            }
            async fn doctor_check(&self) -> anyhow::Result<crate::bead_store::RepairReport> {
                Ok(crate::bead_store::RepairReport::default())
            }
            async fn full_rebuild(&self) -> anyhow::Result<()> {
                Ok(())
            }
            async fn add_dependency(
                &self,
                _bl: &crate::types::BeadId,
                _bd: &crate::types::BeadId,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn remove_dependency(
                &self,
                _blocked_id: &crate::types::BeadId,
                _blocker_id: &crate::types::BeadId,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn claim_auto(&self, _actor: &str) -> anyhow::Result<crate::types::ClaimResult> {
                Ok(crate::types::ClaimResult::NotClaimable {
                    reason: "claim_auto not supported in mock".to_string(),
                })
            }
        }

        let config = ReflectConfig {
            enabled: false,
            ..Default::default()
        };
        let tel = Telemetry::new("test".to_string());
        let dir = tempfile::tempdir().unwrap();
        let strand = ReflectStrand::new(
            config,
            dir.path().to_path_buf(),
            dir.path().join("state"),
            tel,
            None,
        );
        // When disabled, evaluate returns NoWork without touching the store.
        let result = strand.evaluate(&NoOpStore).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn reflect_skips_below_threshold_when_no_state() {
        let config = ReflectConfig {
            enabled: true,
            min_beads_since_last: 100, // very high threshold
            ..Default::default()
        };
        let tel = Telemetry::new("test".to_string());
        let dir = tempfile::tempdir().unwrap();

        // Create a minimal issues.jsonl with 1 closed bead.
        let issues_dir = dir.path().join(".beads");
        std::fs::create_dir_all(&issues_dir).unwrap();
        std::fs::write(
            issues_dir.join("issues.jsonl"),
            r#"{"id":"nd-0001","title":"t","status":"closed","close_reason":"done","closed_at":"2026-04-04T12:00:00Z","assignee":"alpha","labels":[]}"#,
        ).unwrap();

        let strand = ReflectStrand::new(
            config,
            dir.path().to_path_buf(),
            dir.path().join("state"),
            tel,
            None,
        );

        let result = strand.consolidate(false).await.unwrap();
        assert_eq!(result.beads_processed, 0);
        assert_eq!(result.learnings_added, 0);
    }

    #[tokio::test]
    async fn reflect_consolidates_with_force() {
        let config = ReflectConfig {
            enabled: true,
            min_beads_since_last: 100,
            cooldown_hours: 9999,
            ..Default::default()
        };
        let tel = Telemetry::new("test".to_string());
        let dir = tempfile::tempdir().unwrap();

        let issues_dir = dir.path().join(".beads");
        std::fs::create_dir_all(&issues_dir).unwrap();

        let body = r#"Done.\n\n## Retrospective\n- **What worked:** Used the existing pattern\n- **Reusable pattern:** Copy strand template for new strands"#;
        let line = format!(
            r#"{{"id":"nd-0001","title":"t","status":"closed","close_reason":{},"closed_at":"2026-04-04T12:00:00Z","assignee":"alpha","labels":[]}}"#,
            serde_json::to_string(body).unwrap()
        );
        std::fs::write(issues_dir.join("issues.jsonl"), line).unwrap();

        let strand = ReflectStrand::new(
            config,
            dir.path().to_path_buf(),
            dir.path().join("state"),
            tel,
            None,
        );

        let result = strand.consolidate(true).await.unwrap();
        assert_eq!(result.beads_processed, 1);
    }

    #[test]
    fn reflect_state_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let state = ReflectState {
            last_consolidation: Utc::now(),
            beads_at_last_consolidation: 42,
        };
        state.save(dir.path()).unwrap();

        let loaded = ReflectState::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.beads_at_last_consolidation, 42);
    }

    fn make_strand_with_global(
        workspace: &std::path::Path,
        known_workspaces: Vec<std::path::PathBuf>,
        global_path: std::path::PathBuf,
        max_global: usize,
    ) -> ReflectStrand {
        let config = ReflectConfig {
            enabled: true,
            min_beads_since_last: 100,
            cooldown_hours: 9999,
            ..Default::default()
        };
        let tel = Telemetry::new("test".to_string());
        ReflectStrand::new(
            config,
            workspace.to_path_buf(),
            workspace.join("state"),
            tel,
            None,
        )
        .with_global(known_workspaces, global_path, max_global)
    }

    fn write_learnings(workspace: &std::path::Path, entries: &[(&str, &str)]) {
        // entries: (bead_id, observation)
        let beads_dir = workspace.join(".beads");
        std::fs::create_dir_all(&beads_dir).unwrap();
        let mut content = String::from("# Workspace Learnings\n\n");
        for (bead_id, observation) in entries {
            content.push_str(&format!(
                "### 2026-04-04 | bead: {} | worker: alpha | type: other | reinforced: 0\n\
                 - **Observation:** {}\n\
                 - **Confidence:** high\n\
                 - **Source:** test\n\n",
                bead_id, observation
            ));
        }
        std::fs::write(beads_dir.join("learnings.md"), content).unwrap();
    }

    #[test]
    fn cross_workspace_no_known_workspaces_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let global_path = dir.path().join("global-learnings.md");

        let strand = make_strand_with_global(dir.path(), vec![], global_path, 40);
        let learnings = crate::learning::LearningsFile::load(dir.path()).unwrap();

        let promoted = strand.promote_cross_workspace_patterns(&learnings).unwrap();
        assert_eq!(promoted, 0);
    }

    #[test]
    fn cross_workspace_empty_path_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        write_learnings(
            other.path(),
            &[("nd-0001", "use existing pattern from modules")],
        );

        let strand = make_strand_with_global(
            dir.path(),
            vec![other.path().to_path_buf()],
            PathBuf::new(), // empty path
            40,
        );
        let learnings = crate::learning::LearningsFile::load(dir.path()).unwrap();

        let promoted = strand.promote_cross_workspace_patterns(&learnings).unwrap();
        assert_eq!(promoted, 0);
    }

    #[test]
    fn cross_workspace_promotes_matching_entry() {
        let ws1 = tempfile::tempdir().unwrap();
        let ws2 = tempfile::tempdir().unwrap();
        let global_dir = tempfile::tempdir().unwrap();

        // Both workspaces have a similar observation.
        write_learnings(
            ws1.path(),
            &[("nd-0001", "use existing pattern from modules")],
        );
        write_learnings(
            ws2.path(),
            &[("nd-0002", "use existing pattern from modules")],
        );

        let global_path = global_dir.path().join("global-learnings.md");
        let strand = make_strand_with_global(
            ws1.path(),
            vec![ws2.path().to_path_buf()],
            global_path.clone(),
            40,
        );

        let learnings = crate::learning::LearningsFile::load(ws1.path()).unwrap();
        let promoted = strand.promote_cross_workspace_patterns(&learnings).unwrap();

        assert_eq!(promoted, 1);
        assert!(global_path.exists());

        let global = crate::learning::GlobalLearningsFile::load(&global_path).unwrap();
        assert_eq!(global.entries().len(), 1);
        assert!(global.entries()[0]
            .observation
            .contains("use existing pattern"));
    }

    #[test]
    fn cross_workspace_no_match_promotes_nothing() {
        let ws1 = tempfile::tempdir().unwrap();
        let ws2 = tempfile::tempdir().unwrap();
        let global_dir = tempfile::tempdir().unwrap();

        write_learnings(
            ws1.path(),
            &[("nd-0001", "use existing pattern from modules")],
        );
        write_learnings(
            ws2.path(),
            &[("nd-0002", "completely unrelated observation here")],
        );

        let global_path = global_dir.path().join("global-learnings.md");
        let strand = make_strand_with_global(
            ws1.path(),
            vec![ws2.path().to_path_buf()],
            global_path.clone(),
            40,
        );

        let learnings = crate::learning::LearningsFile::load(ws1.path()).unwrap();
        let promoted = strand.promote_cross_workspace_patterns(&learnings).unwrap();

        assert_eq!(promoted, 0);
        assert!(!global_path.exists());
    }

    #[test]
    fn cross_workspace_respects_max_global_learnings() {
        let ws1 = tempfile::tempdir().unwrap();
        let ws2 = tempfile::tempdir().unwrap();
        let global_dir = tempfile::tempdir().unwrap();

        // Three matching entries in each workspace.
        write_learnings(
            ws1.path(),
            &[
                ("nd-0001", "use existing pattern from modules"),
                ("nd-0002", "copy strand template for new strands"),
                ("nd-0003", "run cargo clippy before committing code"),
            ],
        );
        write_learnings(
            ws2.path(),
            &[
                ("nd-0004", "use existing pattern from modules"),
                ("nd-0005", "copy strand template for new strands"),
                ("nd-0006", "run cargo clippy before committing code"),
            ],
        );

        let global_path = global_dir.path().join("global-learnings.md");
        // Cap at 2 entries.
        let strand = make_strand_with_global(
            ws1.path(),
            vec![ws2.path().to_path_buf()],
            global_path.clone(),
            2,
        );

        let learnings = crate::learning::LearningsFile::load(ws1.path()).unwrap();
        let promoted = strand.promote_cross_workspace_patterns(&learnings).unwrap();

        assert_eq!(promoted, 2);
        let global = crate::learning::GlobalLearningsFile::load(&global_path).unwrap();
        assert_eq!(global.entries().len(), 2);
    }

    #[test]
    fn cross_workspace_deduplicates_existing_global() {
        let ws1 = tempfile::tempdir().unwrap();
        let ws2 = tempfile::tempdir().unwrap();
        let global_dir = tempfile::tempdir().unwrap();

        let observation = "use existing pattern from modules";
        write_learnings(ws1.path(), &[("nd-0001", observation)]);
        write_learnings(ws2.path(), &[("nd-0002", observation)]);

        // Pre-populate the global file with the same observation.
        let global_path = global_dir.path().join("global-learnings.md");
        let mut global = crate::learning::GlobalLearningsFile::load(&global_path).unwrap();
        let entry = crate::learning::LearningEntry::new(
            "nd-0000".to_string(),
            "alpha".to_string(),
            crate::learning::BeadType::Other,
            observation.to_string(),
            crate::learning::Confidence::High,
            "pre-existing".to_string(),
        );
        global.promote(entry, 40);
        global.write().unwrap();

        let strand = make_strand_with_global(
            ws1.path(),
            vec![ws2.path().to_path_buf()],
            global_path.clone(),
            40,
        );

        let learnings = crate::learning::LearningsFile::load(ws1.path()).unwrap();
        let promoted = strand.promote_cross_workspace_patterns(&learnings).unwrap();

        // Already in global — should not be added again.
        assert_eq!(promoted, 0);
        let global = crate::learning::GlobalLearningsFile::load(&global_path).unwrap();
        assert_eq!(global.entries().len(), 1);
    }

    // ── Agent extraction tests ──

    /// Mock agent for testing retrospective extraction.
    struct MockReflectAgent {
        /// Fixed retrospective to return (if any).
        retrospective: Option<Retrospective>,
        /// Number of times extract_retrospective was called.
        call_count: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ReflectAgent for MockReflectAgent {
        async fn extract_retrospective(
            &self,
            _bead_title: &str,
            _close_body: &str,
            _workspace: &Path,
        ) -> Result<Option<Retrospective>> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.retrospective.clone())
        }
    }

    #[tokio::test]
    async fn reflect_agent_called_when_no_retrospective() {
        let config = ReflectConfig {
            enabled: true,
            min_beads_since_last: 1,
            cooldown_hours: 0,
            max_extraction_per_run: 10,
            ..Default::default()
        };
        let tel = Telemetry::new("test".to_string());
        let dir = tempfile::tempdir().unwrap();

        let issues_dir = dir.path().join(".beads");
        std::fs::create_dir_all(&issues_dir).unwrap();

        // Bead with no retrospective block.
        std::fs::write(
            issues_dir.join("issues.jsonl"),
            r#"{"id":"nd-0001","title":"Fix bug","status":"closed","close_reason":"Fixed the parsing bug.","closed_at":"2026-04-04T12:00:00Z","assignee":"alpha","labels":[]}"#,
        ).unwrap();

        let retro = Retrospective {
            what_worked: Some("Used the existing pattern".to_string()),
            what_didnt: Some("Nothing".to_string()),
            surprise: Some("N/A".to_string()),
            reusable_pattern: Some("Copy strand template".to_string()),
        };

        let agent = Box::new(MockReflectAgent {
            retrospective: Some(retro),
            call_count: std::sync::atomic::AtomicUsize::new(0),
        });

        let strand = ReflectStrand::new(
            config,
            dir.path().to_path_buf(),
            dir.path().join("state"),
            tel,
            Some(agent),
        );

        let result = strand.consolidate(true).await.unwrap();
        assert_eq!(result.agent_extractions, 1);
        assert_eq!(result.beads_processed, 1);
    }

    #[tokio::test]
    async fn reflect_agent_not_called_when_retrospective_present() {
        let config = ReflectConfig {
            enabled: true,
            min_beads_since_last: 1,
            cooldown_hours: 0,
            max_extraction_per_run: 10,
            ..Default::default()
        };
        let tel = Telemetry::new("test".to_string());
        let dir = tempfile::tempdir().unwrap();

        let issues_dir = dir.path().join(".beads");
        std::fs::create_dir_all(&issues_dir).unwrap();

        // Bead WITH explicit retrospective block.
        let body = r#"Done.\n\n## Retrospective\n- **What worked:** Used the existing pattern\n- **Reusable pattern:** Copy strand template"#;
        let line = format!(
            r#"{{"id":"nd-0001","title":"t","status":"closed","close_reason":{},"closed_at":"2026-04-04T12:00:00Z","assignee":"alpha","labels":[]}}"#,
            serde_json::to_string(body).unwrap()
        );
        std::fs::write(issues_dir.join("issues.jsonl"), line).unwrap();

        // Mock that panics if called (should not be called).
        struct PanicMockAgent;
        #[async_trait::async_trait]
        impl ReflectAgent for PanicMockAgent {
            async fn extract_retrospective(
                &self,
                _bead_title: &str,
                _close_body: &str,
                _workspace: &Path,
            ) -> Result<Option<Retrospective>> {
                panic!("agent should not be called when retrospective is present");
            }
        }

        let strand = ReflectStrand::new(
            config,
            dir.path().to_path_buf(),
            dir.path().join("state"),
            tel,
            Some(Box::new(PanicMockAgent)),
        );

        let result = strand.consolidate(true).await.unwrap();
        assert_eq!(result.agent_extractions, 0);
        assert_eq!(result.beads_processed, 1);
    }

    #[tokio::test]
    async fn reflect_agent_not_called_when_none() {
        let config = ReflectConfig {
            enabled: true,
            min_beads_since_last: 1,
            cooldown_hours: 0,
            max_extraction_per_run: 10,
            ..Default::default()
        };
        let tel = Telemetry::new("test".to_string());
        let dir = tempfile::tempdir().unwrap();

        let issues_dir = dir.path().join(".beads");
        std::fs::create_dir_all(&issues_dir).unwrap();

        // Bead with no retrospective block.
        std::fs::write(
            issues_dir.join("issues.jsonl"),
            r#"{"id":"nd-0001","title":"Fix bug","status":"closed","close_reason":"Fixed the parsing bug.","closed_at":"2026-04-04T12:00:00Z","assignee":"alpha","labels":[]}"#,
        ).unwrap();

        // No agent (None).
        let strand = ReflectStrand::new(
            config,
            dir.path().to_path_buf(),
            dir.path().join("state"),
            tel,
            None,
        );

        let result = strand.consolidate(true).await.unwrap();
        assert_eq!(result.agent_extractions, 0);
        assert_eq!(result.beads_processed, 1);
    }

    #[tokio::test]
    async fn reflect_agent_respects_max_extraction_per_run() {
        let config = ReflectConfig {
            enabled: true,
            min_beads_since_last: 1,
            cooldown_hours: 0,
            max_extraction_per_run: 2,
            ..Default::default()
        };
        let tel = Telemetry::new("test".to_string());
        let dir = tempfile::tempdir().unwrap();

        let issues_dir = dir.path().join(".beads");
        std::fs::create_dir_all(&issues_dir).unwrap();

        // 5 beads, all without retrospectives.
        let mut lines = Vec::new();
        for i in 1..=5 {
            lines.push(format!(
                r#"{{"id":"nd-{:04}","title":"Fix bug {}","status":"closed","close_reason":"Fixed bug {}.","closed_at":"2026-04-04T12:00:00Z","assignee":"alpha","labels":[]}}"#,
                i, i, i
            ));
        }
        std::fs::write(issues_dir.join("issues.jsonl"), lines.join("\n")).unwrap();

        let retro = Retrospective {
            what_worked: Some("Used the existing pattern".to_string()),
            what_didnt: Some("Nothing".to_string()),
            surprise: Some("N/A".to_string()),
            reusable_pattern: Some("Copy strand template".to_string()),
        };

        let agent = Box::new(MockReflectAgent {
            retrospective: Some(retro),
            call_count: std::sync::atomic::AtomicUsize::new(0),
        });

        let strand = ReflectStrand::new(
            config,
            dir.path().to_path_buf(),
            dir.path().join("state"),
            tel,
            Some(agent),
        );

        let result = strand.consolidate(true).await.unwrap();
        // Should only extract 2, not 5 (max_extraction_per_run = 2).
        assert_eq!(result.agent_extractions, 2);
        assert_eq!(result.beads_processed, 5);
    }
}
