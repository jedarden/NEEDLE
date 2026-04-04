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
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::bead_store::BeadStore;
use crate::config::ReflectConfig;
use crate::learning::{
    BeadType, Confidence, GlobalLearningsFile, LearningEntry, LearningsFile, Retrospective,
};
use crate::skill::{render_skill_file, SkillFrontmatter, SkillLibrary};
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{StrandError, StrandResult};

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
}

impl ReflectStrand {
    /// Create a new ReflectStrand.
    ///
    /// - `config`: reflect strand configuration
    /// - `workspace`: workspace root (where `.beads/` lives)
    /// - `state_dir`: directory for persisting reflect state (`~/.needle/state/reflect/`)
    /// - `telemetry`: telemetry emitter
    pub fn new(
        config: ReflectConfig,
        workspace: PathBuf,
        state_dir: PathBuf,
        telemetry: Telemetry,
    ) -> Self {
        ReflectStrand {
            config,
            workspace,
            state_dir,
            telemetry,
            known_workspaces: Vec::new(),
            global_learnings_path: PathBuf::new(),
            max_global_learnings: 40,
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
    pub fn consolidate(&self, force: bool) -> Result<ReflectSummary> {
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

        for bead in &since_last {
            if let Some(reason) = &bead.close_reason {
                match Retrospective::parse_from_close_body(reason) {
                    Ok(Some(retro)) if retro.is_meaningful() => {
                        retro_entries.push((bead.id.clone(), retro));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(bead_id = %bead.id, error = %e, "failed to parse retrospective");
                    }
                }
            }
        }

        tracing::debug!(
            retro_count = retro_entries.len(),
            "reflect: gathered retrospectives"
        );

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

        // Reinforce existing entries that match any closed bead ID, then add
        // non-duplicate candidates up to max_learnings_per_run.
        for bead in &since_last {
            let _ = learnings.reinforce_entry(&bead.id);
        }

        let mut added = 0usize;
        for candidate in candidate_entries {
            if added >= self.config.max_learnings_per_run {
                break;
            }
            // Skip if a similar entry already exists (dedup).
            let similar = learnings.find_similar(&candidate.observation);
            if !similar.is_empty() {
                // Reinforce the most similar entry instead of adding a duplicate.
                let most_similar_id = similar[0].bead_id.clone();
                let _ = learnings.reinforce_entry(&most_similar_id);
                continue;
            }
            learnings.add_entry(candidate)?;
            added += 1;
        }
        summary.learnings_added = added;

        // Promote high-reinforcement entries to skill files.
        let promoted = self.promote_to_skills(learnings.entries())?;
        summary.skills_promoted = promoted;

        // Promote cross-workspace patterns to global learnings.
        let global_promoted = self.promote_cross_workspace_patterns(&learnings)?;
        summary.global_learnings_promoted = global_promoted;

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

        match self.consolidate(false) {
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
    fn reflect_strand_name() {
        let config = ReflectConfig::default();
        let tel = Telemetry::new("test".to_string());
        let strand = ReflectStrand::new(
            config,
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/state"),
            tel,
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
        );
        // When disabled, evaluate returns NoWork without touching the store.
        let result = strand.evaluate(&NoOpStore).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[test]
    fn reflect_skips_below_threshold_when_no_state() {
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
        );

        let result = strand.consolidate(false).unwrap();
        assert_eq!(result.beads_processed, 0);
        assert_eq!(result.learnings_added, 0);
    }

    #[test]
    fn reflect_consolidates_with_force() {
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
        );

        let result = strand.consolidate(true).unwrap();
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
}
