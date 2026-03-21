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

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bead_store::BeadStore;
use crate::config::WeaveConfig;
use crate::types::StrandResult;

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
    agent: Box<dyn WeaveAgent>,
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
    ) -> Self {
        WeaveStrand {
            config,
            workspace,
            state_dir,
            agent,
        }
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

    /// Format documentation files into a string for the prompt.
    fn format_doc_files(files: &[PathBuf], workspace: &Path) -> String {
        if files.is_empty() {
            return "(no documentation files found)".to_string();
        }

        let mut sections = Vec::new();
        for file in files {
            let rel_path = file
                .strip_prefix(workspace)
                .unwrap_or(file)
                .display()
                .to_string();
            match std::fs::read_to_string(file) {
                Ok(content) => {
                    sections.push(format!("### {rel_path}\n\n{}", content.trim_end()));
                }
                Err(_) => {
                    sections.push(format!("### {rel_path}\n\n(failed to read)"));
                }
            }
        }
        sections.join("\n\n")
    }

    /// Format existing beads into a string for the prompt.
    fn format_existing_beads(beads: &[crate::types::Bead]) -> String {
        if beads.is_empty() {
            return "(no open beads)".to_string();
        }

        beads
            .iter()
            .map(|b| {
                let title = &b.title;
                let id = b.id.as_ref();
                let priority = b.priority;
                format!("- [{id}] P{priority}: {title}")
            })
            .collect::<Vec<_>>()
            .join("\n")
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

#[async_trait::async_trait]
impl super::Strand for WeaveStrand {
    fn name(&self) -> &str {
        "weave"
    }

    async fn evaluate(&self, store: &dyn BeadStore) -> StrandResult {
        // Guard: disabled.
        if !self.config.enabled {
            tracing::debug!("weave strand disabled");
            return StrandResult::NoWork;
        }

        // Guard: workspace exclusion.
        if self.is_workspace_excluded() {
            tracing::debug!(
                workspace = %self.workspace.display(),
                "weave strand: workspace excluded"
            );
            return StrandResult::NoWork;
        }

        // Guard: cooldown.
        let state_path = self.state_file_path();
        let mut state = match WeaveState::load(&state_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load weave state, using defaults");
                WeaveState::default()
            }
        };

        if !state.cooldown_elapsed(self.config.cooldown_hours) {
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
            tracing::debug!("weave strand: no documentation files found");
            return StrandResult::NoWork;
        }
        let doc_content = Self::format_doc_files(&doc_files, &self.workspace);

        // Query existing beads for dedup context.
        let existing_beads = match store.list_all().await {
            Ok(beads) => beads,
            Err(e) => {
                tracing::warn!(error = %e, "weave strand: failed to list existing beads");
                return StrandResult::Error(crate::types::StrandError::StoreError(
                    anyhow::anyhow!(e.to_string()),
                ));
            }
        };
        let existing_context = Self::format_existing_beads(&existing_beads);

        // Build prompt and dispatch agent.
        let prompt = self.build_prompt(&doc_content, &existing_context);
        let response = match self.agent.analyze_gaps(&prompt, &self.workspace).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "weave strand: agent dispatch failed");
                return StrandResult::Error(crate::types::StrandError::StoreError(
                    anyhow::anyhow!(e.to_string()),
                ));
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
            StrandResult::WorkCreated
        } else {
            tracing::info!("weave strand: no new beads created (all duplicates or filtered)");
            StrandResult::NoWork
        }
    }
}

// ─── CLI agent implementation ────────────────────────────────────────────────

/// Production `WeaveAgent` that shells out to a CLI agent (e.g., `claude`).
///
/// The agent is invoked in `--print` mode so it emits its analysis as plain
/// text on stdout without tool-use side-effects. The prompt is written to a
/// temp file and fed via stdin redirection.
pub struct CliWeaveAgent {
    /// Agent binary name or path (e.g., `"claude"`).
    agent_cmd: String,
}

impl CliWeaveAgent {
    /// Create a new `CliWeaveAgent`.
    ///
    /// `agent_cmd` is the binary used for analysis (typically taken from
    /// `config.agent.default`).
    pub fn new(agent_cmd: String) -> Self {
        CliWeaveAgent { agent_cmd }
    }
}

#[async_trait::async_trait]
impl WeaveAgent for CliWeaveAgent {
    async fn analyze_gaps(&self, prompt: &str, workspace: &Path) -> Result<String> {
        // Write the prompt to a temp file.
        let tmp_dir = std::env::temp_dir().join("needle");
        std::fs::create_dir_all(&tmp_dir).context("failed to create needle temp dir for weave")?;
        let tmp_file = tmp_dir.join(format!("weave-{}.md", std::process::id()));
        std::fs::write(&tmp_file, prompt).context("failed to write weave prompt to temp file")?;

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
            .with_context(|| format!("failed to spawn weave agent: {}", self.agent_cmd))?;

        // Always clean up the temp file.
        let _ = std::fs::remove_file(&tmp_file);

        if !output.status.success() {
            anyhow::bail!(
                "weave agent exited with code {}",
                output.status.code().unwrap_or(-1)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::{Filters, RepairReport};
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
            workspace: PathBuf::from("/tmp/test"),
            dependencies: vec![],
            dependents: vec![],
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

    use super::super::Strand;

    // ── Tests ────────────────────────────────────────────────────────────

    #[test]
    fn strand_name_is_weave() {
        let dir = tempfile::tempdir().unwrap();
        let strand = WeaveStrand::new(
            WeaveConfig::default(),
            PathBuf::from("/tmp"),
            dir.path().to_path_buf(),
            Box::new(MockAgent::new("NO_GAPS")),
        );
        assert_eq!(strand.name(), "weave");
    }

    #[tokio::test]
    async fn disabled_returns_no_work() {
        let dir = tempfile::tempdir().unwrap();
        let strand = WeaveStrand::new(
            WeaveConfig::default(), // disabled by default
            PathBuf::from("/tmp"),
            dir.path().to_path_buf(),
            Box::new(MockAgent::new("NO_GAPS")),
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store).await;
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
        let strand = WeaveStrand::new(
            config,
            workspace,
            state_dir.path().to_path_buf(),
            Box::new(MockAgent::new("NO_GAPS")),
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store).await;
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
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
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
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store).await;
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
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store).await;
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
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store).await;

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
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store).await;

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
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store).await;

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
        );
        let store = MockStore::new(vec![make_bead("bead-1", "existing task")]);
        let result = strand.evaluate(&store).await;

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
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store).await;
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
        );
        let store = MockStore::empty();
        let _ = strand.evaluate(&store).await;

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
        );
        let store = MockStore::empty();
        let result = strand.evaluate(&store).await;

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
        let path = PathBuf::from("/tmp/nonexistent-weave-state-12345.json");
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
        let result = WeaveStrand::format_doc_files(&[], Path::new("/tmp"));
        assert_eq!(result, "(no documentation files found)");
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
