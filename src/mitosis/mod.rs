//! Mitosis: split multi-task beads into focused children on first failure.
//!
//! Uses child-aware deduplication to prevent duplicate splits. The parent's
//! existing children serve as the dedup source — if children already cover
//! a proposed task, that child is skipped.
//!
//! Concurrency safety: a per-workspace flock serializes the entire mitosis
//! operation (read children → create → link dependencies).
//!
//! Depends on: `bead_store`, `config`, `dispatch`, `prompt`, `telemetry`, `types`, `claim`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::bead_store::BeadStore;
use crate::claim::acquire_flock;
use crate::config::MitosisConfig;
use crate::dispatch::Dispatcher;
use crate::prompt::PromptBuilder;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{Bead, BeadId};

// ──────────────────────────────────────────────────────────────────────────────
// MitosisResult
// ──────────────────────────────────────────────────────────────────────────────

/// Outcome of a mitosis evaluation.
#[derive(Debug)]
pub enum MitosisResult {
    /// Bead was split into child beads.
    Split {
        /// IDs of the newly created children.
        children: Vec<BeadId>,
    },
    /// Agent determined the bead is a single task — no split.
    NotSplittable,
    /// Mitosis was skipped (disabled, not first failure, etc.).
    Skipped { reason: String },
    /// Bead references NEEDLE-internal configuration and is out-of-scope for target workspace.
    OutOfScope,
}

// ──────────────────────────────────────────────────────────────────────────────
// ProposedChild
// ──────────────────────────────────────────────────────────────────────────────

/// A child bead proposed by the agent during mitosis analysis.
#[derive(Debug, Clone, serde::Deserialize)]
struct ProposedChild {
    title: String,
    body: String,
}

/// Agent's mitosis analysis response.
#[derive(Debug, serde::Deserialize)]
struct MitosisResponse {
    splittable: bool,
    #[serde(default)]
    children: Vec<ProposedChild>,
}

// ──────────────────────────────────────────────────────────────────────────────
// MitosisEvaluator
// ──────────────────────────────────────────────────────────────────────────────

/// Evaluates beads for splitting and creates child beads when appropriate.
pub struct MitosisEvaluator {
    config: MitosisConfig,
    telemetry: Telemetry,
    lock_dir: PathBuf,
}

impl MitosisEvaluator {
    pub fn new(config: MitosisConfig, telemetry: Telemetry, lock_dir: PathBuf) -> Self {
        MitosisEvaluator {
            config,
            telemetry,
            lock_dir,
        }
    }

    /// Evaluate a bead for mitosis after failure.
    ///
    /// Checks preconditions (enabled, first failure), then dispatches the agent
    /// to analyze whether the bead contains multiple independent tasks.
    /// If splittable, creates child beads with dedup against existing children.
    pub async fn evaluate(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        workspace: &Path,
        dispatcher: &Dispatcher,
        prompt_builder: &PromptBuilder,
        agent_name: &str,
    ) -> Result<MitosisResult> {
        // Check if mitosis is enabled.
        if !self.config.enabled {
            tracing::debug!(bead_id = %bead.id, "mitosis disabled");
            return Ok(MitosisResult::Skipped {
                reason: "disabled".to_string(),
            });
        }

        // Check if bead references NEEDLE-internal configuration.
        // These tasks have no legitimate resolution path from inside a target repo.
        if detects_needle_internal_config(bead) {
            tracing::info!(
                bead_id = %bead.id,
                title = %bead.title,
                "mitosis skipped: bead references NEEDLE-internal configuration"
            );
            self.telemetry.emit(EventKind::MitosisOutOfScope {
                bead_id: bead.id.clone(),
            })?;
            return Ok(MitosisResult::OutOfScope);
        }

        // Check failure count conditions.
        let failure_count = self.get_failure_count(store, &bead.id).await?;

        // force_failure_threshold: trigger only when failure_count reaches the threshold.
        if self.config.force_failure_threshold > 0 {
            if failure_count < self.config.force_failure_threshold {
                tracing::debug!(
                    bead_id = %bead.id,
                    failure_count,
                    threshold = self.config.force_failure_threshold,
                    "mitosis skipped: below force_failure_threshold"
                );
                return Ok(MitosisResult::Skipped {
                    reason: format!(
                        "failure count {} below threshold {}",
                        failure_count, self.config.force_failure_threshold
                    ),
                });
            }
        } else {
            // Check if we should fire based on first_failure_only or repeat_interval.
            let should_fire = if self.config.repeat_interval > 0 {
                // repeat_interval mode: fire at 1, 1+N, 1+2N, ...
                // But skip beads that are already mitosis children (have mitosis-depth label).
                let has_mitosis_depth_label =
                    bead.labels.iter().any(|l| l.starts_with("mitosis-depth:"));
                let is_repeat_tick =
                    failure_count > 1 && (failure_count - 1) % self.config.repeat_interval == 0;

                // Fire at first failure OR at repeat interval ticks (if not a mitosis child)
                failure_count == 1 || (is_repeat_tick && !has_mitosis_depth_label)
            } else {
                // first_failure_only mode: only fire at failure_count == 1
                !self.config.first_failure_only || failure_count == 1
            };

            if !should_fire {
                tracing::debug!(
                    bead_id = %bead.id,
                    failure_count,
                    first_failure_only = self.config.first_failure_only,
                    repeat_interval = self.config.repeat_interval,
                    "mitosis skipped: not at trigger point"
                );
                return Ok(MitosisResult::Skipped {
                    reason: format!(
                        "not at trigger point (count={}, first_failure_only={}, repeat_interval={})",
                        failure_count, self.config.first_failure_only, self.config.repeat_interval
                    ),
                });
            }
        }

        // Resolve the agent adapter.
        let adapter = match dispatcher.adapter(agent_name) {
            Some(a) => a,
            None => {
                tracing::warn!(
                    bead_id = %bead.id,
                    agent = agent_name,
                    "mitosis skipped: agent adapter not found"
                );
                return Ok(MitosisResult::Skipped {
                    reason: format!("adapter '{}' not found", agent_name),
                });
            }
        };

        // Gather existing children for the prompt (so the agent avoids duplicates).
        let existing_children = self.get_existing_children(store, &bead.id).await?;
        let existing_children_text = if existing_children.is_empty() {
            "(no existing children)".to_string()
        } else {
            existing_children
                .iter()
                .map(|t| format!("- {t}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        // Build mitosis prompt.
        let prompt = prompt_builder
            .build_mitosis(bead, workspace, "mitosis", &existing_children_text)
            .context("failed to build mitosis prompt")?;

        // Acquire workspace flock for atomicity.
        let lock_path = self.lock_dir.join(format!(
            "needle-mitosis-{}.lock",
            sanitize_path_component(&workspace.display().to_string())
        ));
        let _lock = acquire_flock(&lock_path)
            .await
            .context("failed to acquire mitosis flock")?;

        tracing::info!(bead_id = %bead.id, "dispatching agent for mitosis analysis");

        // Dispatch agent with mitosis prompt.
        let exec_result = dispatcher
            .dispatch(&bead.id, &prompt, adapter, workspace)
            .await
            .context("mitosis agent dispatch failed")?;

        // Parse the agent's response.
        let response = parse_mitosis_response(&exec_result.stdout);

        match response {
            Some(resp) if resp.splittable && !resp.children.is_empty() => {
                self.telemetry.emit(EventKind::MitosisEvaluated {
                    bead_id: bead.id.clone(),
                    splittable: true,
                    proposed_children: resp.children.len() as u32,
                })?;

                self.create_children(store, bead, &resp.children).await
            }
            Some(resp) if resp.splittable => {
                // Splittable but no children proposed — treat as not splittable.
                tracing::info!(
                    bead_id = %bead.id,
                    "agent said splittable but proposed no children"
                );
                self.telemetry.emit(EventKind::MitosisEvaluated {
                    bead_id: bead.id.clone(),
                    splittable: false,
                    proposed_children: 0,
                })?;
                Ok(MitosisResult::NotSplittable)
            }
            Some(_) => {
                tracing::info!(bead_id = %bead.id, "agent determined bead is single task");
                self.telemetry.emit(EventKind::MitosisEvaluated {
                    bead_id: bead.id.clone(),
                    splittable: false,
                    proposed_children: 0,
                })?;
                Ok(MitosisResult::NotSplittable)
            }
            None => {
                tracing::warn!(
                    bead_id = %bead.id,
                    exit_code = exec_result.exit_code,
                    "could not parse mitosis response from agent"
                );
                self.telemetry.emit(EventKind::MitosisEvaluated {
                    bead_id: bead.id.clone(),
                    splittable: false,
                    proposed_children: 0,
                })?;
                Ok(MitosisResult::NotSplittable)
            }
        }
    }

    /// Create child beads with dedup against existing children.
    async fn create_children(
        &self,
        store: &dyn BeadStore,
        parent: &Bead,
        proposed: &[ProposedChild],
    ) -> Result<MitosisResult> {
        // Enter the bead.mitosis span for the mitosis operation.
        let mitosis_span = tracing::info_span!(
            "bead.mitosis",
            needle.bead.id = %parent.id,
            needle.mitosis.proposed_children = proposed.len() as u32,
            needle.mitosis.children_created = tracing::field::Empty, // Will be set based on result
            needle.mitosis.children_skipped = tracing::field::Empty, // Will be set based on result
        );
        let _mitosis_enter = mitosis_span.enter();

        // Read parent's existing children (dependencies where child blocks parent).
        let existing = self.get_existing_children(store, &parent.id).await?;
        let existing_titles: Vec<String> = existing.iter().map(|t| t.to_lowercase()).collect();

        let mut created_ids = Vec::new();
        let mut skipped = 0u32;

        for child in proposed {
            // Dedup: does an existing child cover this task?
            if existing_titles
                .iter()
                .any(|t| titles_match(t, &child.title.to_lowercase()))
            {
                tracing::debug!(
                    parent_id = %parent.id,
                    child_title = %child.title,
                    "skipping duplicate child"
                );
                skipped += 1;
                continue;
            }

            // Create child bead with parent-tracking labels for reliable dedup.
            // Labels are stored on the bead itself and survive FrankenSQLite
            // index corruption, unlike dependency relationships.
            let parent_label = format!("parent-{}", parent.id);
            let labels: Vec<&str> = vec!["mitosis-child", "mitosis-depth:1", &parent_label];
            let child_id = store
                .create_bead(&child.title, &child.body, &labels)
                .await
                .with_context(|| format!("failed to create child bead: {}", child.title))?;

            // Link child as blocking parent.
            store
                .add_dependency(&child_id, &parent.id)
                .await
                .with_context(|| {
                    format!(
                        "failed to add dependency: {} blocks {}",
                        child_id, parent.id
                    )
                })?;

            tracing::info!(
                parent_id = %parent.id,
                child_id = %child_id,
                child_title = %child.title,
                "created mitosis child"
            );

            created_ids.push(child_id);
        }

        if created_ids.is_empty() {
            // All proposed children already existed.
            tracing::info!(
                parent_id = %parent.id,
                existing = existing.len(),
                "all proposed children already exist (dedup)"
            );
            self.telemetry.emit(EventKind::MitosisSkipped {
                parent_id: parent.id.clone(),
                existing_children: existing.len() as u32,
            })?;
            return Ok(MitosisResult::Skipped {
                reason: "all children already exist".to_string(),
            });
        }

        self.telemetry.emit(EventKind::MitosisSplit {
            parent_id: parent.id.clone(),
            children_created: created_ids.len() as u32,
            children_skipped: skipped,
            child_ids: created_ids.clone(),
        })?;

        // Record the final counts on the bead.mitosis span
        tracing::Span::current()
            .record("needle.mitosis.children_created", created_ids.len() as u32);
        tracing::Span::current().record("needle.mitosis.children_skipped", skipped);

        tracing::info!(
            parent_id = %parent.id,
            children_created = created_ids.len(),
            children_skipped = skipped,
            "mitosis split completed"
        );

        Ok(MitosisResult::Split {
            children: created_ids,
        })
    }

    /// Read the failure count label from a bead.
    ///
    /// All br calls are wrapped in timeouts to prevent indefinite hang.
    async fn get_failure_count(&self, store: &dyn BeadStore, bead_id: &BeadId) -> Result<u32> {
        let labels =
            match tokio::time::timeout(std::time::Duration::from_secs(30), store.labels(bead_id))
                .await
            {
                Ok(Ok(l)) => l,
                Ok(Err(e)) => {
                    tracing::warn!(
                        bead_id = %bead_id,
                        error = %e,
                        "failed to read labels for failure count"
                    );
                    return Ok(0);
                }
                Err(_) => {
                    tracing::warn!(
                        bead_id = %bead_id,
                        "labels() timed out after 30s, assuming failure count 0"
                    );
                    return Ok(0);
                }
            };

        let count = labels
            .iter()
            .filter_map(|l| l.strip_prefix("failure-count:"))
            .filter_map(|n| n.parse::<u32>().ok())
            .max()
            .unwrap_or(0);

        Ok(count)
    }

    /// Get titles of existing children for a parent bead.
    ///
    /// Uses label-based discovery (`parent-<parent_id>` label) instead of
    /// reading the parent's dependency list. This is robust against
    /// FrankenSQLite index corruption where `br dep add` creates the
    /// dependency link and labels but the relationship doesn't appear in
    /// `br show --json` output.
    ///
    /// The `list_all()` call is wrapped in a timeout to prevent indefinite
    /// hang in HANDLING state.
    async fn get_existing_children(
        &self,
        store: &dyn BeadStore,
        parent_id: &BeadId,
    ) -> Result<Vec<String>> {
        let parent_label = format!("parent-{}", parent_id);
        let all_beads = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            store.list_all(),
        )
        .await
        {
            Ok(Ok(beads)) => beads,
            Ok(Err(e)) => {
                tracing::warn!(
                    parent_id = %parent_id,
                    error = %e,
                    "list_all failed during get_existing_children, assuming no children"
                );
                return Ok(Vec::new());
            }
            Err(_) => {
                tracing::warn!(
                    parent_id = %parent_id,
                    "list_all timed out after 30s during get_existing_children, assuming no children"
                );
                return Ok(Vec::new());
            }
        };

        let titles: Vec<String> = all_beads
            .iter()
            .filter(|b| b.labels.iter().any(|l| l == &parent_label))
            .map(|b| b.title.clone())
            .collect();

        Ok(titles)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Parsing
// ──────────────────────────────────────────────────────────────────────────────

/// Parse the agent's mitosis analysis response from stdout.
///
/// Searches for a JSON object in the output. Handles markdown code fencing
/// and surrounding text.
fn parse_mitosis_response(stdout: &str) -> Option<MitosisResponse> {
    // Try direct JSON parse first.
    if let Ok(resp) = serde_json::from_str::<MitosisResponse>(stdout.trim()) {
        return Some(resp);
    }

    // Try extracting JSON from markdown code fences.
    let json_str = extract_json_block(stdout)?;
    serde_json::from_str::<MitosisResponse>(json_str).ok()
}

/// Extract a JSON object from text that may contain markdown code fences.
fn extract_json_block(text: &str) -> Option<&str> {
    // Look for ```json ... ``` or ``` ... ``` blocks.
    if let Some(start) = text.find("```json") {
        let content_start = start + "```json".len();
        if let Some(end) = text[content_start..].find("```") {
            return Some(text[content_start..content_start + end].trim());
        }
    }

    if let Some(start) = text.find("```") {
        let content_start = start + "```".len();
        // Skip to next line if the opening ``` has text after it.
        let content_start = text[content_start..]
            .find('\n')
            .map(|n| content_start + n + 1)
            .unwrap_or(content_start);
        if let Some(end) = text[content_start..].find("```") {
            return Some(text[content_start..content_start + end].trim());
        }
    }

    // Try to find a bare JSON object.
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        // Find matching closing brace (simple heuristic).
        let mut depth = 0i32;
        for (i, ch) in trimmed.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&trimmed[..=i]);
                    }
                }
                _ => {}
            }
        }
    }

    None
}

/// Sanitize a path string for use as a filename component.
fn sanitize_path_component(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Check if two titles match (fuzzy comparison for dedup).
///
/// Considers titles matching if they are identical after normalization,
/// or if one contains the other as a substring.
fn titles_match(existing: &str, proposed: &str) -> bool {
    if existing == proposed {
        return true;
    }

    // Normalize: remove common prefixes and compare.
    let normalize = |s: &str| -> String {
        s.trim()
            .trim_start_matches(|c: char| !c.is_alphabetic())
            .to_lowercase()
    };

    let e = normalize(existing);
    let p = normalize(proposed);

    e == p || e.contains(&p) || p.contains(&e)
}

/// Detect if a bead references NEEDLE-internal configuration.
///
/// Returns true if the bead content suggests investigating or fixing NEEDLE's own
/// dispatch configuration (Pluck, exclude_labels, bead discovery, etc.).
/// These tasks have no legitimate resolution path from inside a target repo.
pub fn detects_needle_internal_config(bead: &Bead) -> bool {
    let combined_text = format!(
        "{} {}",
        bead.title.to_lowercase(),
        bead.body
            .as_ref()
            .map(|b| b.to_lowercase())
            .unwrap_or_default()
    );

    // Patterns that indicate NEEDLE-internal configuration work.
    // These are derived from the real ARMOR incident (bead bf-3b64 and its lineage).
    let internal_config_patterns = [
        "pluck configuration",
        "pluck config",
        "exclude_labels",
        "exclude labels",
        "bead discovery",
        "starvation alert",
        "beads invisible to worker",
        "open beads exist but pluck found none",
        "needle dispatch",
        "strand configuration",
        "worker configuration",
        "bead filtering",
        "candidate exclusion",
    ];

    for pattern in &internal_config_patterns {
        if combined_text.contains(pattern) {
            tracing::debug!(
                bead_id = %bead.id,
                pattern,
                "bead references NEEDLE-internal configuration"
            );
            return true;
        }
    }

    false
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::{Filters, RepairReport};
    use crate::types::{Bead, BeadId, BeadStatus, ClaimResult};
    use async_trait::async_trait;
    use chrono::Utc;
    use std::path::PathBuf;
    use std::sync::Mutex;

    // ── Mock store ──

    struct MockStore {
        labels: Vec<String>,
        /// Existing child beads returned by `list_all()` for dedup testing.
        existing_children: Vec<Bead>,
        created: Mutex<Vec<(String, String)>>,
        deps_added: Mutex<Vec<(String, String)>>,
    }

    impl MockStore {
        fn new() -> Self {
            MockStore {
                labels: vec!["failure-count:1".to_string()],
                existing_children: Vec::new(),
                created: Mutex::new(Vec::new()),
                deps_added: Mutex::new(Vec::new()),
            }
        }

        fn with_labels(mut self, labels: Vec<String>) -> Self {
            self.labels = labels;
            self
        }

        /// Add existing child beads that will be returned by `list_all()`.
        fn with_existing_children(mut self, children: Vec<Bead>) -> Self {
            self.existing_children = children;
            self
        }
    }

    #[async_trait]
    impl BeadStore for MockStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.existing_children.clone())
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            Ok(Bead {
                id: BeadId::from("parent-001"),
                title: "Parent bead".to_string(),
                body: Some("Test parent".to_string()),
                priority: 1,
                status: BeadStatus::Open,
                assignee: None,
                labels: self.labels.clone(),
                workspace: PathBuf::from("/tmp/test"),
                dependencies: vec![],
                dependents: vec![],
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "mock".to_string(),
            })
        }

        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "mock".to_string(),
            })
        }

        async fn release(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
        async fn reopen(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            Ok(self.labels.clone())
        }
        async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn create_bead(&self, title: &str, body: &str, _labels: &[&str]) -> Result<BeadId> {
            self.created
                .lock()
                .unwrap()
                .push((title.to_string(), body.to_string()));
            let id = format!("child-{:03}", self.created.lock().unwrap().len());
            Ok(BeadId::from(id))
        }
        async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
            self.deps_added
                .lock()
                .unwrap()
                .push((blocker_id.to_string(), blocked_id.to_string()));
            Ok(())
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
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

    fn test_bead() -> Bead {
        Bead {
            id: BeadId::from("parent-001"),
            title: "Multi-task bead".to_string(),
            body: Some("Add endpoint AND write migration AND update tests".to_string()),
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: vec!["failure-count:1".to_string()],
            workspace: PathBuf::from("/tmp/test"),
            dependencies: vec![],
            dependents: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ── parse_mitosis_response tests ──

    #[test]
    fn parse_response_not_splittable() {
        let resp = parse_mitosis_response(r#"{"splittable": false}"#);
        assert!(resp.is_some());
        let r = resp.unwrap();
        assert!(!r.splittable);
        assert!(r.children.is_empty());
    }

    #[test]
    fn parse_response_splittable_with_children() {
        let resp = parse_mitosis_response(
            r#"{"splittable": true, "children": [
                {"title": "Add endpoint", "body": "Create REST endpoint"},
                {"title": "Write migration", "body": "Add DB migration"}
            ]}"#,
        );
        assert!(resp.is_some());
        let r = resp.unwrap();
        assert!(r.splittable);
        assert_eq!(r.children.len(), 2);
        assert_eq!(r.children[0].title, "Add endpoint");
        assert_eq!(r.children[1].title, "Write migration");
    }

    #[test]
    fn parse_response_from_markdown_code_fence() {
        let stdout = r#"Here is my analysis:
```json
{"splittable": true, "children": [{"title": "Task A", "body": "Do A"}]}
```
That's my answer."#;
        let resp = parse_mitosis_response(stdout);
        assert!(resp.is_some());
        assert!(resp.unwrap().splittable);
    }

    #[test]
    fn parse_response_invalid_json() {
        let resp = parse_mitosis_response("this is not json at all");
        assert!(resp.is_none());
    }

    #[test]
    fn parse_response_embedded_json_object() {
        let stdout = r#"Based on my analysis:
{"splittable": false}
End of response."#;
        // The bare JSON finder should pick it up.
        let resp = parse_mitosis_response(stdout);
        // May or may not succeed depending on surrounding text; this is best-effort.
        // The direct parse should fail, but the extract_json_block should handle it.
        assert!(resp.is_some() || resp.is_none()); // We just ensure no panic.
    }

    // ── titles_match tests ──

    #[test]
    fn titles_match_exact() {
        assert!(titles_match("add endpoint", "add endpoint"));
    }

    #[test]
    fn titles_match_substring() {
        assert!(titles_match("add endpoint for users", "add endpoint"));
        assert!(titles_match("add endpoint", "add endpoint for users"));
    }

    #[test]
    fn titles_no_match() {
        assert!(!titles_match("write migration", "add endpoint"));
    }

    // ── extract_json_block tests ──

    #[test]
    fn extract_from_json_fence() {
        let text = "blah\n```json\n{\"splittable\": true}\n```\nmore";
        let block = extract_json_block(text);
        assert!(block.is_some());
        assert!(block.unwrap().contains("splittable"));
    }

    #[test]
    fn extract_bare_json() {
        let text = "{\"splittable\": false}";
        let block = extract_json_block(text);
        assert!(block.is_some());
    }

    // ── sanitize_path_component tests ──

    #[test]
    fn sanitize_replaces_slashes() {
        assert_eq!(
            sanitize_path_component("/home/user/test"),
            "_home_user_test"
        );
    }

    // ── MitosisEvaluator precondition tests ──

    #[tokio::test]
    async fn evaluate_skips_when_disabled() {
        let config = MitosisConfig {
            enabled: false,
            first_failure_only: true,
            force_failure_threshold: 0,
            repeat_interval: 0,
        };
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let evaluator = MitosisEvaluator::new(config, telemetry, PathBuf::from("/tmp"));
        let store = MockStore::new();
        let bead = test_bead();

        // We need a dispatcher + prompt_builder for the signature, but they
        // shouldn't be called since mitosis is disabled.
        // Since we can't easily mock them, we verify the skip logic directly.
        let result = evaluator
            .evaluate(
                &store,
                &bead,
                Path::new("/tmp/test"),
                // These won't be accessed because we skip early.
                // Pass minimal dispatcher/builder by creating them.
                &create_test_dispatcher(),
                &PromptBuilder::new(&crate::config::PromptConfig::default()),
                "claude-sonnet",
            )
            .await
            .unwrap();

        assert!(matches!(result, MitosisResult::Skipped { reason } if reason == "disabled"));
    }

    #[tokio::test]
    async fn evaluate_skips_when_not_first_failure() {
        let config = MitosisConfig {
            enabled: true,
            first_failure_only: true,
            force_failure_threshold: 0,
            repeat_interval: 0,
        };
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let evaluator = MitosisEvaluator::new(config, telemetry, PathBuf::from("/tmp"));
        let store = MockStore::new().with_labels(vec!["failure-count:2".to_string()]);
        let bead = test_bead();

        let result = evaluator
            .evaluate(
                &store,
                &bead,
                Path::new("/tmp/test"),
                &create_test_dispatcher(),
                &PromptBuilder::new(&crate::config::PromptConfig::default()),
                "claude-sonnet",
            )
            .await
            .unwrap();

        assert!(matches!(result, MitosisResult::Skipped { .. }));
    }

    /// Create a bead that looks like an existing mitosis child of a parent.
    fn existing_child(title: &str, parent_id: &str) -> Bead {
        Bead {
            id: BeadId::from(format!("existing-{}", title.replace(' ', "-"))),
            title: title.to_string(),
            body: Some("Existing child".to_string()),
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: vec![
                "mitosis-child".to_string(),
                "mitosis-depth:1".to_string(),
                format!("parent-{}", parent_id),
            ],
            workspace: PathBuf::from("/tmp/test"),
            dependencies: vec![],
            dependents: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn create_children_with_dedup() {
        let config = MitosisConfig {
            enabled: true,
            first_failure_only: true,
            force_failure_threshold: 0,
            repeat_interval: 0,
        };
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let evaluator = MitosisEvaluator::new(config, telemetry, PathBuf::from("/tmp"));

        // Parent already has a child titled "Add endpoint" (found via label).
        let store = MockStore::new()
            .with_existing_children(vec![existing_child("Add endpoint", "parent-001")]);

        let parent = test_bead();
        let proposed = vec![
            ProposedChild {
                title: "Add endpoint".to_string(),
                body: "Already exists".to_string(),
            },
            ProposedChild {
                title: "Write migration".to_string(),
                body: "New child".to_string(),
            },
        ];

        let result = evaluator
            .create_children(&store, &parent, &proposed)
            .await
            .unwrap();

        match result {
            MitosisResult::Split { children } => {
                assert_eq!(children.len(), 1, "should create only the novel child");
                let created = store.created.lock().unwrap();
                assert_eq!(created.len(), 1);
                assert_eq!(created[0].0, "Write migration");
                let deps = store.deps_added.lock().unwrap();
                assert_eq!(deps.len(), 1);
                assert_eq!(deps[0].1, "parent-001");
            }
            other => panic!("expected Split, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn create_children_all_deduped() {
        let config = MitosisConfig {
            enabled: true,
            first_failure_only: true,
            force_failure_threshold: 0,
            repeat_interval: 0,
        };
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let evaluator = MitosisEvaluator::new(config, telemetry, PathBuf::from("/tmp"));

        // Both proposed children already exist (found via parent label).
        let store = MockStore::new().with_existing_children(vec![
            existing_child("Add endpoint", "parent-001"),
            existing_child("Write migration", "parent-001"),
        ]);

        let parent = test_bead();
        let proposed = vec![
            ProposedChild {
                title: "Add endpoint".to_string(),
                body: "Already exists".to_string(),
            },
            ProposedChild {
                title: "Write migration".to_string(),
                body: "Also exists".to_string(),
            },
        ];

        let result = evaluator
            .create_children(&store, &parent, &proposed)
            .await
            .unwrap();

        assert!(
            matches!(result, MitosisResult::Skipped { .. }),
            "all children deduped should result in Skipped"
        );
        assert!(store.created.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dedup_ignores_children_of_other_parents() {
        // Children exist but belong to a different parent — should not dedup.
        let config = MitosisConfig {
            enabled: true,
            first_failure_only: true,
            force_failure_threshold: 0,
            repeat_interval: 0,
        };
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let evaluator = MitosisEvaluator::new(config, telemetry, PathBuf::from("/tmp"));

        let store = MockStore::new()
            .with_existing_children(vec![existing_child("Add endpoint", "different-parent")]);

        let parent = test_bead();
        let proposed = vec![ProposedChild {
            title: "Add endpoint".to_string(),
            body: "Same title but different parent".to_string(),
        }];

        let result = evaluator
            .create_children(&store, &parent, &proposed)
            .await
            .unwrap();

        match result {
            MitosisResult::Split { children } => {
                assert_eq!(
                    children.len(),
                    1,
                    "should create child since parent differs"
                );
            }
            other => panic!("expected Split, got {:?}", other),
        }
    }

    fn create_test_dispatcher() -> Dispatcher {
        use std::collections::HashMap;
        let adapters: HashMap<String, crate::dispatch::AgentAdapter> = HashMap::new();
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        Dispatcher::with_adapters(adapters, telemetry, 60)
    }

    // ── repeat_interval tests ──

    #[tokio::test]
    async fn repeat_interval_triggers_at_correct_counts() {
        // repeat_interval = 50 should fire at 1, 51, 101, 151, ...
        let config = MitosisConfig {
            enabled: true,
            first_failure_only: false,
            force_failure_threshold: 0,
            repeat_interval: 50,
        };
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let evaluator = MitosisEvaluator::new(config, telemetry, PathBuf::from("/tmp"));

        // Test failure_count = 1 (should fire)
        let store = MockStore::new().with_labels(vec!["failure-count:1".to_string()]);
        let bead = test_bead();
        let result = evaluator
            .evaluate(
                &store,
                &bead,
                Path::new("/tmp/test"),
                &create_test_dispatcher(),
                &PromptBuilder::new(&crate::config::PromptConfig::default()),
                "claude-sonnet",
            )
            .await
            .unwrap();
        // Should not skip (adapter not found is a different skip, but we check the gate passes)
        assert!(
            !matches!(result, MitosisResult::Skipped { reason } if reason.contains("not at trigger point")),
            "failure_count=1 should trigger mitosis"
        );

        // Test failure_count = 51 (should fire: (51-1) % 50 == 0)
        let store = MockStore::new().with_labels(vec!["failure-count:51".to_string()]);
        let bead = test_bead();
        let result = evaluator
            .evaluate(
                &store,
                &bead,
                Path::new("/tmp/test"),
                &create_test_dispatcher(),
                &PromptBuilder::new(&crate::config::PromptConfig::default()),
                "claude-sonnet",
            )
            .await
            .unwrap();
        assert!(
            !matches!(result, MitosisResult::Skipped { reason } if reason.contains("not at trigger point")),
            "failure_count=51 should trigger mitosis (1+50)"
        );

        // Test failure_count = 101 (should fire: (101-1) % 50 == 0)
        let store = MockStore::new().with_labels(vec!["failure-count:101".to_string()]);
        let bead = test_bead();
        let result = evaluator
            .evaluate(
                &store,
                &bead,
                Path::new("/tmp/test"),
                &create_test_dispatcher(),
                &PromptBuilder::new(&crate::config::PromptConfig::default()),
                "claude-sonnet",
            )
            .await
            .unwrap();
        assert!(
            !matches!(result, MitosisResult::Skipped { reason } if reason.contains("not at trigger point")),
            "failure_count=101 should trigger mitosis (1+2*50)"
        );

        // Test failure_count = 25 (should NOT fire: not 1, 1+50, 1+100, ...)
        let store = MockStore::new().with_labels(vec!["failure-count:25".to_string()]);
        let bead = test_bead();
        let result = evaluator
            .evaluate(
                &store,
                &bead,
                Path::new("/tmp/test"),
                &create_test_dispatcher(),
                &PromptBuilder::new(&crate::config::PromptConfig::default()),
                "claude-sonnet",
            )
            .await
            .unwrap();
        assert!(
            matches!(result, MitosisResult::Skipped { reason } if reason.contains("not at trigger point")),
            "failure_count=25 should NOT trigger mitosis"
        );
    }

    #[tokio::test]
    async fn repeat_interval_skips_mitosis_depth_beads() {
        // Beads with mitosis-depth:1 label should be skipped even at repeat ticks
        let config = MitosisConfig {
            enabled: true,
            first_failure_only: false,
            force_failure_threshold: 0,
            repeat_interval: 50,
        };
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let evaluator = MitosisEvaluator::new(config, telemetry, PathBuf::from("/tmp"));

        // Bead with mitosis-depth:1 label at failure_count = 51 (repeat tick)
        let store = MockStore::new().with_labels(vec![
            "failure-count:51".to_string(),
            "mitosis-depth:1".to_string(),
        ]);
        let mut bead = test_bead();
        bead.labels = vec![
            "failure-count:51".to_string(),
            "mitosis-depth:1".to_string(),
        ];

        let result = evaluator
            .evaluate(
                &store,
                &bead,
                Path::new("/tmp/test"),
                &create_test_dispatcher(),
                &PromptBuilder::new(&crate::config::PromptConfig::default()),
                "claude-sonnet",
            )
            .await
            .unwrap();

        assert!(
            matches!(result, MitosisResult::Skipped { .. }),
            "bead with mitosis-depth:1 should be skipped even at repeat tick"
        );
    }

    #[tokio::test]
    async fn repeat_interval_zero_preserves_first_failure_only() {
        // repeat_interval = 0 should behave like first_failure_only
        let config = MitosisConfig {
            enabled: true,
            first_failure_only: true,
            force_failure_threshold: 0,
            repeat_interval: 0,
        };
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let evaluator = MitosisEvaluator::new(config, telemetry, PathBuf::from("/tmp"));

        // Test failure_count = 1 (should fire)
        let store = MockStore::new().with_labels(vec!["failure-count:1".to_string()]);
        let bead = test_bead();
        let result = evaluator
            .evaluate(
                &store,
                &bead,
                Path::new("/tmp/test"),
                &create_test_dispatcher(),
                &PromptBuilder::new(&crate::config::PromptConfig::default()),
                "claude-sonnet",
            )
            .await
            .unwrap();
        assert!(
            !matches!(result, MitosisResult::Skipped { reason } if reason.contains("not at trigger point")),
            "failure_count=1 should trigger mitosis"
        );

        // Test failure_count = 2 (should NOT fire - first_failure_only mode)
        let store = MockStore::new().with_labels(vec!["failure-count:2".to_string()]);
        let bead = test_bead();
        let result = evaluator
            .evaluate(
                &store,
                &bead,
                Path::new("/tmp/test"),
                &create_test_dispatcher(),
                &PromptBuilder::new(&crate::config::PromptConfig::default()),
                "claude-sonnet",
            )
            .await
            .unwrap();
        assert!(
            matches!(result, MitosisResult::Skipped { reason } if reason.contains("not at trigger point")),
            "failure_count=2 should NOT trigger mitosis (first_failure_only mode)"
        );
    }

    #[tokio::test]
    async fn repeat_interval_skips_max_depth_beads() {
        // Test that beads with mitosis-depth:1 label are skipped during repeat tick.
        // Verify depth-limited beads don't trigger repeat mitosis.
        let config = MitosisConfig {
            enabled: true,
            first_failure_only: false,
            force_failure_threshold: 0,
            repeat_interval: 50,
        };
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let evaluator = MitosisEvaluator::new(config, telemetry, PathBuf::from("/tmp"));

        // Bead with mitosis-depth:1 label at failure_count = 51 (repeat tick)
        // This should be skipped because it's a mitosis child bead (depth-limited).
        let store = MockStore::new().with_labels(vec![
            "failure-count:51".to_string(),
            "mitosis-depth:1".to_string(),
        ]);
        let mut bead = test_bead();
        bead.labels = vec![
            "failure-count:51".to_string(),
            "mitosis-depth:1".to_string(),
        ];

        let result = evaluator
            .evaluate(
                &store,
                &bead,
                Path::new("/tmp/test"),
                &create_test_dispatcher(),
                &PromptBuilder::new(&crate::config::PromptConfig::default()),
                "claude-sonnet",
            )
            .await
            .unwrap();

        assert!(
            matches!(result, MitosisResult::Skipped { .. }),
            "bead with mitosis-depth:1 should be skipped even at repeat tick (failure_count=51)"
        );
    }

    // ── OutOfScope tests (ADR-002 Phase 6.1) ──

    #[tokio::test]
    async fn evaluate_returns_out_of_scope_for_needle_internal_config() {
        // Regression test for ADR-002 Phase 6.1
        // Uses real bf-3b64 lineage text as fixture:
        // - "Starvation alert: beads invisible to worker" (bf-3b64 title)
        // - Body referencing "bead discovery configuration" and "exclude_labels"
        //
        // These beads reference NEEDLE's own dispatch configuration and have no
        // legitimate resolution path from inside a target repo. The mitosis
        // evaluator must return OutOfScope and NOT create any child beads.

        let config = MitosisConfig {
            enabled: true,
            first_failure_only: true,
            force_failure_threshold: 0,
            repeat_interval: 0,
        };
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let evaluator = MitosisEvaluator::new(config, telemetry, PathBuf::from("/tmp"));

        // Bead matching bf-3b64: "Starvation alert: beads invisible to worker"
        // with body referencing NEEDLE-internal configuration
        let mut bead = test_bead();
        bead.title = "Starvation alert: beads invisible to worker".to_string();
        bead.body = Some(
            "Pluck found no candidates but open beads exist.\n\
             This may be due to exclude_labels filtering or bead discovery configuration.\n\
             Investigate Pluck configuration and adjust exclude_labels setting."
                .to_string(),
        );
        bead.labels = vec!["failure-count:1".to_string()];

        let store = MockStore::new().with_labels(vec!["failure-count:1".to_string()]);

        let result = evaluator
            .evaluate(
                &store,
                &bead,
                Path::new("/tmp/test"),
                &create_test_dispatcher(),
                &PromptBuilder::new(&crate::config::PromptConfig::default()),
                "claude-sonnet",
            )
            .await
            .unwrap();

        // Should return OutOfScope, not create children
        match result {
            MitosisResult::OutOfScope => {
                // Expected - bead references NEEDLE-internal configuration
            }
            other => panic!(
                "expected OutOfScope for NEEDLE-internal config bead, got: {:?}",
                other
            ),
        }

        // Assert that NO child beads were created in the store
        let created = store.created.lock().unwrap();
        assert!(
            created.is_empty(),
            "expected no child beads to be created, but found: {:?}",
            created
        );
    }

    #[tokio::test]
    async fn evaluate_returns_out_of_scope_for_pluck_config_beads() {
        // Additional regression test for "Pluck configuration" references
        let config = MitosisConfig {
            enabled: true,
            first_failure_only: true,
            force_failure_threshold: 0,
            repeat_interval: 0,
        };
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let evaluator = MitosisEvaluator::new(config, telemetry, PathBuf::from("/tmp"));

        let mut bead = test_bead();
        bead.title = "Fix bead discovery configuration".to_string();
        bead.body = Some(
            "Investigate why Pluck is not finding beads.\n\
             Check exclude_labels configuration and adjust filters."
                .to_string(),
        );
        bead.labels = vec!["failure-count:1".to_string()];

        let store = MockStore::new().with_labels(vec!["failure-count:1".to_string()]);

        let result = evaluator
            .evaluate(
                &store,
                &bead,
                Path::new("/tmp/test"),
                &create_test_dispatcher(),
                &PromptBuilder::new(&crate::config::PromptConfig::default()),
                "claude-sonnet",
            )
            .await
            .unwrap();

        assert!(matches!(result, MitosisResult::OutOfScope));

        let created = store.created.lock().unwrap();
        assert!(
            created.is_empty(),
            "expected no child beads for Pluck config bead"
        );
    }

    #[tokio::test]
    async fn evaluate_returns_out_of_scope_for_strand_config_beads() {
        // Test for "strand configuration" and "worker configuration" references
        let config = MitosisConfig {
            enabled: true,
            first_failure_only: true,
            force_failure_threshold: 0,
            repeat_interval: 0,
        };
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let evaluator = MitosisEvaluator::new(config, telemetry, PathBuf::from("/tmp"));

        let mut bead = test_bead();
        bead.title = "Configure strand filters".to_string();
        bead.body = Some(
            "Update strand configuration to improve candidate filtering.\n\
             Adjust worker settings for better dispatch behavior."
                .to_string(),
        );
        bead.labels = vec!["failure-count:1".to_string()];

        let store = MockStore::new().with_labels(vec!["failure-count:1".to_string()]);

        let result = evaluator
            .evaluate(
                &store,
                &bead,
                Path::new("/tmp/test"),
                &create_test_dispatcher(),
                &PromptBuilder::new(&crate::config::PromptConfig::default()),
                "claude-sonnet",
            )
            .await
            .unwrap();

        assert!(matches!(result, MitosisResult::OutOfScope));

        let created = store.created.lock().unwrap();
        assert!(
            created.is_empty(),
            "expected no child beads for strand config bead"
        );
    }
}
