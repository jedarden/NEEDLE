//! Unravel strand: propose automated alternatives for human-blocked beads.
//!
//! When beads carry the "human" label (requiring a human decision before
//! proceeding), Unravel dispatches an AI agent to propose workarounds that
//! could be executed autonomously. Alternative proposals are created as
//! child beads of the original human-labeled bead.
//!
//! **Guardrails:**
//! - Opt-in only (disabled by default).
//! - Max beads analyzed per run.
//! - Max alternatives per bead.
//! - 7-day cooldown per bead (already-analyzed beads are skipped).
//! - Original human bead is never modified or closed.
//!
//! Depends on: `bead_store`, `config`, `telemetry`, `types`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bead_store::BeadStore;
use crate::config::UnravelConfig;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{Bead, BeadId, StrandResult};

// ─── UnravelAgent trait ─────────────────────────────────────────────────────

/// Abstraction for agent invocation used by the Unravel strand.
///
/// Production implementations wrap the `Dispatcher`; tests use mocks.
#[async_trait::async_trait]
pub trait UnravelAgent: Send + Sync {
    /// Invoke an agent with the given prompt, returning its raw text response.
    async fn propose_alternatives(&self, prompt: &str, workspace: &Path) -> Result<String>;
}

// ─── Proposed alternative (parsed from agent response) ──────────────────────

/// An alternative approach proposed by the agent for a human-blocked bead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedAlternative {
    pub title: String,
    pub body: String,
}

// ─── Persistent state ───────────────────────────────────────────────────────

/// Persisted state for the Unravel strand.
///
/// Tracks which beads have been analyzed and when, enforcing the cooldown.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UnravelState {
    /// Map of bead ID -> last analysis timestamp.
    pub analyzed: HashMap<String, DateTime<Utc>>,
}

impl UnravelState {
    /// Load state from disk, returning default if file doesn't exist.
    fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(data) => serde_json::from_str(&data)
                .with_context(|| format!("failed to parse unravel state: {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => {
                Err(e).with_context(|| format!("failed to read unravel state: {}", path.display()))
            }
        }
    }

    /// Persist state to disk.
    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create state dir: {}", parent.display()))?;
        }
        let data =
            serde_json::to_string_pretty(self).context("failed to serialize unravel state")?;
        std::fs::write(path, data)
            .with_context(|| format!("failed to write unravel state: {}", path.display()))
    }

    /// Check if a bead is within the cooldown period.
    fn is_in_cooldown(&self, bead_id: &BeadId, cooldown_hours: i64) -> bool {
        match self.analyzed.get(bead_id.as_ref()) {
            None => false,
            Some(last) => {
                let elapsed = Utc::now().signed_duration_since(*last);
                elapsed.num_hours() < cooldown_hours
            }
        }
    }

    /// Record that a bead has been analyzed.
    fn mark_analyzed(&mut self, bead_id: &BeadId) {
        self.analyzed.insert(bead_id.to_string(), Utc::now());
    }
}

// ─── UnravelStrand ─────────────────────────────────────────────────────────

/// The Unravel strand — proposes automated alternatives for human-blocked beads.
pub struct UnravelStrand {
    config: UnravelConfig,
    workspace: PathBuf,
    state_dir: PathBuf,
    agent: Box<dyn UnravelAgent>,
    telemetry: Telemetry,
}

impl UnravelStrand {
    /// Create a new UnravelStrand.
    ///
    /// `state_dir` is the base directory for unravel state files
    /// (e.g., `~/.needle/state/unravel/`).
    pub fn new(
        config: UnravelConfig,
        workspace: PathBuf,
        state_dir: PathBuf,
        agent: Box<dyn UnravelAgent>,
        telemetry: Telemetry,
    ) -> Self {
        UnravelStrand {
            config,
            workspace,
            state_dir,
            agent,
            telemetry,
        }
    }

    /// Compute the state file path for a workspace.
    fn state_file_path(&self) -> PathBuf {
        let hash = workspace_hash(&self.workspace);
        self.state_dir.join(format!("{hash}.json"))
    }

    /// Find beads with the "human" label from all beads in the store.
    fn filter_human_beads(beads: &[Bead]) -> Vec<&Bead> {
        beads
            .iter()
            .filter(|b| b.labels.iter().any(|l| l == "human"))
            .collect()
    }

    /// Get existing child beads (unravel-proposal children) for a parent bead.
    fn count_existing_children(bead: &Bead) -> u32 {
        bead.dependencies
            .iter()
            .filter(|d| d.dependency_type == "blocks")
            .count() as u32
    }

    /// Build the unravel prompt for a human-blocked bead.
    ///
    /// Uses `config.prompt_template` when set; otherwise falls back to the
    /// built-in template.  Template variables: `{id}`, `{title}`, `{body}`,
    /// `{labels}`.
    fn build_prompt(&self, bead: &Bead) -> String {
        let body = bead.body.as_deref().unwrap_or("(no description)");
        let labels = bead.labels.join(", ");

        if let Some(template) = &self.config.prompt_template {
            return template
                .replace("{id}", bead.id.as_ref())
                .replace("{title}", &bead.title)
                .replace("{body}", body)
                .replace("{labels}", &labels);
        }

        format!(
            "## Human-Blocked Bead\n\n\
             **ID:** {id}\n\
             **Title:** {title}\n\
             **Description:**\n\
             {body}\n\
             **Labels:** {labels}\n\n\
             ## Task\n\n\
             This bead is blocked because it requires a human decision. \
             Propose automated alternatives that could accomplish the same \
             goal without human intervention.\n\n\
             For each alternative, provide:\n\
             - **title**: concise description of the alternative approach\n\
             - **body**: what needs to be done (actionable, an agent can execute it)\n\n\
             Constraints:\n\
             - Alternatives must be fully automatable (no human decisions needed)\n\
             - Each alternative should be a self-contained, actionable task\n\
             - Propose only viable alternatives, not partial workarounds\n\n\
             Output a JSON array of objects with \"title\" and \"body\" fields.\n\
             If no viable automated alternative exists, respond with: NO_ALTERNATIVES",
            id = bead.id,
            title = bead.title,
            body = body,
            labels = labels,
        )
    }

    /// Parse the agent response into proposed alternatives.
    pub fn parse_agent_response(response: &str) -> Result<Vec<ProposedAlternative>> {
        let trimmed = response.trim();

        // Check for NO_ALTERNATIVES sentinel.
        if trimmed.contains("NO_ALTERNATIVES") {
            return Ok(vec![]);
        }

        // Try parsing as a JSON array directly.
        if let Ok(alts) = serde_json::from_str::<Vec<ProposedAlternative>>(trimmed) {
            return Ok(alts);
        }

        // Try extracting JSON from markdown code fences.
        let json_str = extract_json_block(trimmed).unwrap_or(trimmed);

        // Try as array.
        if let Ok(alts) = serde_json::from_str::<Vec<ProposedAlternative>>(json_str) {
            return Ok(alts);
        }

        // Try as object with "alternatives" field.
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(json_str) {
            if let Some(arr) = obj.get("alternatives").and_then(|v| v.as_array()) {
                let alts: Vec<ProposedAlternative> = arr
                    .iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                    .collect();
                if !alts.is_empty() {
                    return Ok(alts);
                }
            }
        }

        anyhow::bail!("failed to parse agent response as proposed alternatives")
    }
}

#[async_trait::async_trait]
impl super::Strand for UnravelStrand {
    fn name(&self) -> &str {
        "unravel"
    }

    async fn evaluate(&self, store: &dyn BeadStore) -> StrandResult {
        // Guard: disabled.
        if !self.config.enabled {
            tracing::debug!("unravel strand disabled");
            return StrandResult::NoWork;
        }

        // Load persistent state.
        let state_path = self.state_file_path();
        let mut state = match UnravelState::load(&state_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load unravel state, using defaults");
                UnravelState::default()
            }
        };

        // Query all beads and filter for human-labeled ones.
        let all_beads = match store.list_all().await {
            Ok(beads) => beads,
            Err(e) => {
                tracing::warn!(error = %e, "unravel strand: failed to list beads");
                return StrandResult::Error(crate::types::StrandError::StoreError(
                    anyhow::anyhow!(e.to_string()),
                ));
            }
        };

        let human_beads = Self::filter_human_beads(&all_beads);
        if human_beads.is_empty() {
            tracing::debug!("unravel strand: no human-labeled beads found");
            return StrandResult::NoWork;
        }

        tracing::info!(
            human_beads = human_beads.len(),
            "unravel strand: found human-labeled beads"
        );

        // Process beads with guardrails.
        let mut total_created = 0u32;
        let mut beads_processed = 0u32;
        let cooldown_hours = self.config.cooldown_hours as i64;

        for bead in &human_beads {
            // Guard: max beads per run.
            if beads_processed >= self.config.max_beads_per_run {
                tracing::info!(
                    max = self.config.max_beads_per_run,
                    "unravel strand: max beads per run reached"
                );
                break;
            }

            // Guard: cooldown — skip recently analyzed beads.
            if state.is_in_cooldown(&bead.id, cooldown_hours) {
                tracing::debug!(
                    bead_id = %bead.id,
                    "unravel strand: bead is in cooldown, skipping"
                );
                self.telemetry
                    .emit(EventKind::UnravelSkipped {
                        bead_id: bead.id.clone(),
                        reason: "cooldown".to_string(),
                    })
                    .ok();
                continue;
            }

            // Guard: max alternatives per bead — check existing children.
            let existing_children = Self::count_existing_children(bead);
            if existing_children >= self.config.max_alternatives_per_bead {
                tracing::debug!(
                    bead_id = %bead.id,
                    existing_children,
                    max = self.config.max_alternatives_per_bead,
                    "unravel strand: bead already has max alternatives, skipping"
                );
                state.mark_analyzed(&bead.id);
                continue;
            }

            let remaining_slots = self.config.max_alternatives_per_bead - existing_children;
            beads_processed += 1;

            // Build prompt and dispatch agent.
            let prompt = self.build_prompt(bead);
            let response = match self
                .agent
                .propose_alternatives(&prompt, &self.workspace)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %e,
                        "unravel strand: agent dispatch failed"
                    );
                    state.mark_analyzed(&bead.id);
                    continue;
                }
            };

            // Parse proposed alternatives.
            let proposed = match Self::parse_agent_response(&response) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %e,
                        "unravel strand: failed to parse agent response"
                    );
                    state.mark_analyzed(&bead.id);
                    continue;
                }
            };

            if proposed.is_empty() {
                tracing::info!(
                    bead_id = %bead.id,
                    "unravel strand: no alternatives proposed"
                );
                state.mark_analyzed(&bead.id);
                continue;
            }

            // Create child beads as alternatives.
            let mut created_for_this_bead = 0u32;
            for alternative in &proposed {
                // Guard: remaining slots for this bead.
                if created_for_this_bead >= remaining_slots {
                    break;
                }

                // Guard: max total created per run.
                if total_created >= self.config.max_beads_per_run {
                    break;
                }

                let child_title = format!("[Unravel] {} — {}", bead.title, alternative.title);
                let child_body = format!(
                    "## Alternative for: {}\n\
                     Original bead: {id}\n\n\
                     {alt_body}\n\n\
                     ---\n\
                     This is an automated alternative proposal created by the \
                     unravel strand. It is a child of the original human-blocked \
                     bead and can be executed independently.",
                    bead.title,
                    id = bead.id,
                    alt_body = alternative.body,
                );
                let labels: Vec<&str> = vec!["unravel-proposal"];

                match store.create_bead(&child_title, &child_body, &labels).await {
                    Ok(child_id) => {
                        // Link child as blocking parent (child blocks original).
                        if let Err(e) = store.add_dependency(&child_id, &bead.id).await {
                            tracing::warn!(
                                bead_id = %bead.id,
                                child_id = %child_id,
                                error = %e,
                                "unravel strand: failed to add dependency"
                            );
                        } else {
                            tracing::info!(
                                parent_id = %bead.id,
                                child_id = %child_id,
                                child_title = %child_title,
                                "unravel strand: created alternative child bead"
                            );
                        }

                        created_for_this_bead += 1;
                        total_created += 1;
                    }
                    Err(e) => {
                        tracing::warn!(
                            bead_id = %bead.id,
                            error = %e,
                            title = %alternative.title,
                            "unravel strand: failed to create child bead"
                        );
                    }
                }
            }

            // Emit telemetry for this bead.
            self.telemetry
                .emit(EventKind::UnravelAnalyzed {
                    bead_id: bead.id.clone(),
                    alternatives_proposed: created_for_this_bead,
                })
                .ok();

            state.mark_analyzed(&bead.id);
        }

        // Save state.
        if let Err(e) = state.save(&state_path) {
            tracing::warn!(error = %e, "unravel strand: failed to save state");
        }

        if total_created > 0 {
            tracing::info!(
                created = total_created,
                "unravel strand: created alternative beads for human-blocked beads"
            );
            StrandResult::WorkCreated
        } else {
            tracing::info!("unravel strand: no new alternatives created");
            StrandResult::NoWork
        }
    }
}

// ─── Production agent ────────────────────────────────────────────────────────

/// Production `UnravelAgent` that invokes the configured AI agent via subprocess.
///
/// The agent binary is called with `--print` so it emits the response as plain
/// text on stdout without tool-use side-effects.  The prompt is written to a
/// temp file and fed via stdin redirection.
pub struct CliUnravelAgent {
    /// Agent binary name or path (e.g., `"claude"`).
    agent_cmd: String,
}

impl CliUnravelAgent {
    /// Create a new `CliUnravelAgent`.
    ///
    /// `agent_cmd` is the binary used for analysis (typically taken from
    /// `config.agent.default`).
    pub fn new(agent_cmd: String) -> Self {
        CliUnravelAgent { agent_cmd }
    }
}

#[async_trait::async_trait]
impl UnravelAgent for CliUnravelAgent {
    async fn propose_alternatives(&self, prompt: &str, workspace: &Path) -> Result<String> {
        // Write the prompt to a temp file.
        let tmp_dir = std::env::temp_dir().join("needle");
        std::fs::create_dir_all(&tmp_dir)
            .context("failed to create needle temp dir for unravel")?;
        let tmp_file = tmp_dir.join(format!("unravel-{}.md", std::process::id()));
        std::fs::write(&tmp_file, prompt).context("failed to write unravel prompt to temp file")?;

        // Build the shell command: cd into workspace, pipe prompt to agent.
        let cmd = format!(
            "cd {} && {} --print < {}",
            workspace.display(),
            self.agent_cmd,
            tmp_file.display(),
        );

        let output = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(&cmd)
            .output()
            .await
            .with_context(|| format!("failed to spawn unravel agent: {}", self.agent_cmd))?;

        // Always clean up the temp file.
        let _ = std::fs::remove_file(&tmp_file);

        if !output.status.success() {
            anyhow::bail!(
                "unravel agent exited with code {}",
                output.status.code().unwrap_or(-1)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Extract a JSON block from markdown-fenced content.
fn extract_json_block(text: &str) -> Option<&str> {
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
    result
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

// ─── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::{Filters, RepairReport};
    use crate::types::{BeadStatus, ClaimResult};

    use anyhow::Result;
    use chrono::{TimeZone, Utc};
    use std::sync::Mutex;

    // ── Mock UnravelAgent ──────────────────────────────────────────────

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
    impl UnravelAgent for MockAgent {
        async fn propose_alternatives(&self, _prompt: &str, _workspace: &Path) -> Result<String> {
            Ok(self.response.lock().unwrap().clone())
        }
    }

    struct FailingAgent;

    #[async_trait::async_trait]
    impl UnravelAgent for FailingAgent {
        async fn propose_alternatives(&self, _prompt: &str, _workspace: &Path) -> Result<String> {
            anyhow::bail!("agent dispatch failed")
        }
    }

    // ── Mock BeadStore ──────────────────────────────────────────────────

    struct MockStore {
        beads: Vec<Bead>,
        created: Mutex<Vec<(String, String, Vec<String>)>>,
        deps_added: Mutex<Vec<(String, String)>>,
    }

    impl MockStore {
        fn new(beads: Vec<Bead>) -> Self {
            MockStore {
                beads,
                created: Mutex::new(Vec::new()),
                deps_added: Mutex::new(Vec::new()),
            }
        }

        fn empty() -> Self {
            Self::new(vec![])
        }

        fn created_beads(&self) -> Vec<(String, String, Vec<String>)> {
            self.created.lock().unwrap().clone()
        }

        fn deps(&self) -> Vec<(String, String)> {
            self.deps_added.lock().unwrap().clone()
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
            Ok(ClaimResult::NotClaimable {
                reason: "mock".to_string(),
            })
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
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
            let id = format!("unravel-{}", self.created.lock().unwrap().len());
            Ok(BeadId::from(id))
        }
        async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
            self.deps_added
                .lock()
                .unwrap()
                .push((blocker_id.to_string(), blocked_id.to_string()));
            Ok(())
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
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn make_bead(id: &str, title: &str, labels: &[&str]) -> Bead {
        let dt = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        Bead {
            id: BeadId::from(id.to_string()),
            title: title.to_string(),
            body: Some(format!("Description for {title}")),
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            workspace: PathBuf::from("/tmp/test"),
            dependencies: vec![],
            dependents: vec![],
            created_at: dt,
            updated_at: dt,
        }
    }

    fn make_enabled_config() -> UnravelConfig {
        UnravelConfig {
            enabled: true,
            max_beads_per_run: 5,
            max_alternatives_per_bead: 3,
            cooldown_hours: 168, // 7 days
            ..UnravelConfig::default()
        }
    }

    use super::super::Strand;

    // ── Tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn strand_name_is_unravel() {
        let dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());
        let strand = UnravelStrand::new(
            UnravelConfig::default(),
            PathBuf::from("/tmp"),
            dir.path().to_path_buf(),
            Box::new(MockAgent::new("NO_ALTERNATIVES")),
            telemetry,
        );
        assert_eq!(strand.name(), "unravel");
    }

    #[tokio::test]
    async fn disabled_returns_no_work() {
        let dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());
        let strand = UnravelStrand::new(
            UnravelConfig::default(), // disabled by default
            PathBuf::from("/tmp"),
            dir.path().to_path_buf(),
            Box::new(MockAgent::new("NO_ALTERNATIVES")),
            telemetry,
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn no_human_beads_returns_no_work() {
        let dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());
        let strand = UnravelStrand::new(
            make_enabled_config(),
            PathBuf::from("/tmp"),
            dir.path().to_path_buf(),
            Box::new(MockAgent::new("NO_ALTERNATIVES")),
            telemetry,
        );
        let store = MockStore::new(vec![make_bead("nd-1", "Normal task", &[])]);
        let result = strand.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn creates_alternative_children() {
        let _dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());
        let state_dir = tempfile::tempdir().unwrap();

        let response = r#"[
            {"title": "Automated approach A", "body": "Do A automatically"},
            {"title": "Automated approach B", "body": "Do B automatically"}
        ]"#;

        let strand = UnravelStrand::new(
            make_enabled_config(),
            PathBuf::from("/tmp"),
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            telemetry,
        );

        let human_bead = make_bead("nd-human", "Needs human review", &["human"]);
        let store = MockStore::new(vec![human_bead]);
        let result = strand.evaluate(&store).await;

        assert!(
            matches!(result, StrandResult::WorkCreated),
            "should return WorkCreated; got {:?}",
            result
        );

        let created = store.created_beads();
        assert_eq!(created.len(), 2, "should create 2 alternative beads");
        assert!(created[0].2.contains(&"unravel-proposal".to_string()));
        assert!(created[1].2.contains(&"unravel-proposal".to_string()));

        let deps = store.deps();
        assert_eq!(deps.len(), 2, "should add 2 dependencies");
        // Each child blocks the parent.
        for (_child, parent) in &deps {
            assert_eq!(parent, "nd-human", "child should block parent");
        }
    }

    #[tokio::test]
    async fn respects_max_beads_per_run() {
        let _dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());
        let state_dir = tempfile::tempdir().unwrap();

        let config = UnravelConfig {
            max_beads_per_run: 1,
            ..make_enabled_config()
        };

        let response = r#"[{"title": "Alt A", "body": "Do A"}]"#;
        let strand = UnravelStrand::new(
            config,
            PathBuf::from("/tmp"),
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            telemetry,
        );

        let bead1 = make_bead("nd-h1", "Human bead 1", &["human"]);
        let bead2 = make_bead("nd-h2", "Human bead 2", &["human"]);
        let store = MockStore::new(vec![bead1, bead2]);
        let _ = strand.evaluate(&store).await;

        let created = store.created_beads();
        assert_eq!(created.len(), 1, "should only process 1 bead");
    }

    #[tokio::test]
    async fn respects_max_alternatives_per_bead() {
        let _dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());
        let state_dir = tempfile::tempdir().unwrap();

        let config = UnravelConfig {
            max_alternatives_per_bead: 1,
            ..make_enabled_config()
        };

        let response = r#"[
            {"title": "Alt A", "body": "Do A"},
            {"title": "Alt B", "body": "Do B"}
        ]"#;
        let strand = UnravelStrand::new(
            config,
            PathBuf::from("/tmp"),
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            telemetry,
        );

        let human_bead = make_bead("nd-human", "Needs review", &["human"]);
        let store = MockStore::new(vec![human_bead]);
        let _ = strand.evaluate(&store).await;

        let created = store.created_beads();
        assert_eq!(created.len(), 1, "should only create 1 alternative");
    }

    #[tokio::test]
    async fn cooldown_skips_recently_analyzed() {
        let state_dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());

        // Pre-populate state with recent analysis.
        let mut state = UnravelState::default();
        state.mark_analyzed(&BeadId::from("nd-cooldown".to_string()));
        let hash = workspace_hash(Path::new("/tmp/test"));
        let state_path = state_dir.path().join(format!("{hash}.json"));
        state.save(&state_path).unwrap();

        let config = UnravelConfig {
            cooldown_hours: 168,
            ..make_enabled_config()
        };

        let response = r#"[{"title": "Alt", "body": "body"}]"#;
        let strand = UnravelStrand::new(
            config,
            PathBuf::from("/tmp/test"),
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            telemetry,
        );

        let human_bead = make_bead("nd-cooldown", "In cooldown", &["human"]);
        let store = MockStore::new(vec![human_bead]);
        let result = strand.evaluate(&store).await;

        assert!(
            matches!(result, StrandResult::NoWork),
            "should skip bead in cooldown"
        );
        assert!(store.created_beads().is_empty());
    }

    #[tokio::test]
    async fn cooldown_elapsed_after_7_days() {
        let state_dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());

        // Pre-populate state with old analysis (8 days ago).
        let mut state = UnravelState::default();
        let _bead_id = BeadId::from("nd-expired".to_string());
        state.analyzed.insert(
            "nd-expired".to_string(),
            Utc::now() - chrono::Duration::days(8),
        );
        let hash = workspace_hash(Path::new("/tmp/test"));
        let state_path = state_dir.path().join(format!("{hash}.json"));
        state.save(&state_path).unwrap();

        let config = UnravelConfig {
            cooldown_hours: 168, // 7 days
            ..make_enabled_config()
        };

        let response = r#"[{"title": "Alt", "body": "body"}]"#;
        let strand = UnravelStrand::new(
            config,
            PathBuf::from("/tmp/test"),
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            telemetry,
        );

        let human_bead = make_bead("nd-expired", "Cooldown expired", &["human"]);
        let store = MockStore::new(vec![human_bead]);
        let result = strand.evaluate(&store).await;

        assert!(
            matches!(result, StrandResult::WorkCreated),
            "should process bead after cooldown"
        );
    }

    #[tokio::test]
    async fn no_alternatives_returns_no_work() {
        let state_dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());

        let strand = UnravelStrand::new(
            make_enabled_config(),
            PathBuf::from("/tmp"),
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new("NO_ALTERNATIVES")),
            telemetry,
        );

        let human_bead = make_bead("nd-noalt", "No alts possible", &["human"]);
        let store = MockStore::new(vec![human_bead]);
        let result = strand.evaluate(&store).await;

        assert!(matches!(result, StrandResult::NoWork));
        assert!(store.created_beads().is_empty());
    }

    #[tokio::test]
    async fn agent_failure_skips_bead() {
        let state_dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());

        let strand = UnravelStrand::new(
            make_enabled_config(),
            PathBuf::from("/tmp"),
            state_dir.path().to_path_buf(),
            Box::new(FailingAgent),
            telemetry,
        );

        let human_bead = make_bead("nd-fail", "Agent fails", &["human"]);
        let store = MockStore::new(vec![human_bead]);
        let result = strand.evaluate(&store).await;

        assert!(matches!(result, StrandResult::NoWork));
        assert!(store.created_beads().is_empty());
    }

    #[tokio::test]
    async fn original_bead_never_modified() {
        let state_dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());

        let response = r#"[{"title": "Alt", "body": "body"}]"#;
        let strand = UnravelStrand::new(
            make_enabled_config(),
            PathBuf::from("/tmp"),
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            telemetry,
        );

        let human_bead = make_bead("nd-original", "Original human bead", &["human"]);
        let store = MockStore::new(vec![human_bead]);
        let _ = strand.evaluate(&store).await;

        // Verify no modifications to the original bead (no labels changed, etc.)
        // The MockStore only tracks creates and deps — no modifications to existing beads.
        // This test validates the strand only creates new beads and deps.
        assert_eq!(store.created_beads().len(), 1);
        assert_eq!(store.deps().len(), 1);
    }

    #[tokio::test]
    async fn state_persisted_after_run() {
        let state_dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());

        let response = r#"[{"title": "Alt", "body": "body"}]"#;
        let strand = UnravelStrand::new(
            make_enabled_config(),
            PathBuf::from("/tmp/test"),
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new(response)),
            telemetry,
        );

        let human_bead = make_bead("nd-state", "State test", &["human"]);
        let store = MockStore::new(vec![human_bead]);
        let _ = strand.evaluate(&store).await;

        // Verify state was saved.
        let hash = workspace_hash(Path::new("/tmp/test"));
        let state_path = state_dir.path().join(format!("{hash}.json"));
        let state = UnravelState::load(&state_path).unwrap();
        assert!(
            state.analyzed.contains_key("nd-state"),
            "analyzed bead should be tracked"
        );
    }

    // ── Parse tests ─────────────────────────────────────────────────────

    #[test]
    fn parse_no_alternatives() {
        let result = UnravelStrand::parse_agent_response("NO_ALTERNATIVES").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_json_array() {
        let input = r#"[{"title": "Auto approach", "body": "Do X"}]"#;
        let result = UnravelStrand::parse_agent_response(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Auto approach");
    }

    #[test]
    fn parse_json_object_with_alternatives_key() {
        let input = r#"{"alternatives": [{"title": "Auto approach", "body": "Do X"}]}"#;
        let result = UnravelStrand::parse_agent_response(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Auto approach");
    }

    #[test]
    fn parse_fenced_json() {
        let input = "```json\n[{\"title\": \"Fenced alt\", \"body\": \"body\"}]\n```";
        let result = UnravelStrand::parse_agent_response(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Fenced alt");
    }

    #[test]
    fn parse_invalid_json_returns_error() {
        let result = UnravelStrand::parse_agent_response("not json at all");
        assert!(result.is_err());
    }

    // ── State tests ─────────────────────────────────────────────────────

    #[test]
    fn state_cooldown_not_in_cooldown_when_new() {
        let state = UnravelState::default();
        assert!(!state.is_in_cooldown(&BeadId::from("any"), 168));
    }

    #[test]
    fn state_cooldown_in_cooldown_when_recent() {
        let mut state = UnravelState::default();
        state.mark_analyzed(&BeadId::from("nd-test"));
        assert!(state.is_in_cooldown(&BeadId::from("nd-test"), 168));
    }

    #[test]
    fn state_cooldown_elapsed_after_period() {
        let mut state = UnravelState::default();
        let id = BeadId::from("nd-old");
        state
            .analyzed
            .insert("nd-old".to_string(), Utc::now() - chrono::Duration::days(8));
        assert!(!state.is_in_cooldown(&id, 168)); // 7 days
    }

    #[test]
    fn state_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let mut state = UnravelState::default();
        state.mark_analyzed(&BeadId::from("bead-1"));
        state.mark_analyzed(&BeadId::from("bead-2"));
        state.save(&path).unwrap();

        let loaded = UnravelState::load(&path).unwrap();
        assert!(loaded.analyzed.contains_key("bead-1"));
        assert!(loaded.analyzed.contains_key("bead-2"));
    }

    #[test]
    fn state_load_missing_file_returns_default() {
        let path = PathBuf::from("/tmp/nonexistent-unravel-state-12345.json");
        let state = UnravelState::load(&path).unwrap();
        assert!(state.analyzed.is_empty());
    }

    // ── Filter tests ────────────────────────────────────────────────────

    #[test]
    fn filter_human_beads() {
        let beads = vec![
            make_bead("nd-1", "Normal", &[]),
            make_bead("nd-2", "Human task", &["human"]),
            make_bead("nd-3", "Deferred", &["deferred"]),
            make_bead("nd-4", "Human + other", &["human", "priority"]),
        ];
        let filtered = UnravelStrand::filter_human_beads(&beads);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].id, BeadId::from("nd-2"));
        assert_eq!(filtered[1].id, BeadId::from("nd-4"));
    }

    // ── Default config tests ────────────────────────────────────────────

    #[test]
    fn default_config_is_disabled() {
        let config = UnravelConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_beads_per_run, 5);
        assert_eq!(config.max_alternatives_per_bead, 3);
        assert_eq!(config.cooldown_hours, 168);
    }

    // ── Workspace hash test ─────────────────────────────────────────────

    #[test]
    fn workspace_hash_is_deterministic() {
        let h1 = workspace_hash(Path::new("/home/user/project"));
        let h2 = workspace_hash(Path::new("/home/user/project"));
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    // ── Custom prompt template tests ─────────────────────────────────────

    #[tokio::test]
    async fn custom_prompt_template_substitutes_variables() {
        let config = UnravelConfig {
            enabled: true,
            prompt_template: Some("ID={id} TITLE={title} BODY={body} LABELS={labels}".to_string()),
            ..UnravelConfig::default()
        };
        let strand = UnravelStrand::new(
            config,
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/state"),
            Box::new(MockAgent::new("NO_ALTERNATIVES")),
            Telemetry::new("test".to_string()),
        );
        let bead = make_bead("nd-1", "Fix it", &["human", "urgent"]);
        let prompt = strand.build_prompt(&bead);
        assert!(prompt.contains("ID=nd-1"));
        assert!(prompt.contains("TITLE=Fix it"));
        assert!(prompt.contains("BODY=Description for Fix it"));
        assert!(prompt.contains("LABELS=human, urgent"));
    }

    #[tokio::test]
    async fn default_prompt_template_contains_key_sections() {
        let strand = UnravelStrand::new(
            make_enabled_config(),
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/state"),
            Box::new(MockAgent::new("NO_ALTERNATIVES")),
            Telemetry::new("test".to_string()),
        );
        let bead = make_bead("nd-2", "Approve deployment", &["human"]);
        let prompt = strand.build_prompt(&bead);
        assert!(prompt.contains("nd-2"));
        assert!(prompt.contains("Approve deployment"));
        assert!(prompt.contains("NO_ALTERNATIVES"));
        assert!(prompt.contains("JSON array"));
    }
}
