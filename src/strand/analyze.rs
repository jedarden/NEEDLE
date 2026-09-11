//! Analyze strand: the escalation ladder's rung-4 analysis dispatch.
//!
//! Phase 19 (docs/plan/plan.md §19.2, ADR-022 decision 3): a bead that has
//! exhausted its three quarantine rounds does not loop back into normal
//! redispatch. Exactly one *plan-grounded analysis dispatch* re-reads the
//! workspace plan (`docs/plan/plan.md`) together with the bead's full failure
//! trail and must conclude with exactly one of:
//!
//! - **(a) re-scope** — a single child bead created as a blocker of the
//!   original, labelled `split-child` and `escalation:rescoped`, leaving the
//!   original open and dependency-blocked; or
//! - **(b) human** — the `human` label plus an `analysis:` note stating what
//!   the plan does not answer (rung 5, Principle 7).
//!
//! The handler rejects every other outcome. A dispatch that yields neither a
//! valid re-scope nor a valid `human`-with-analysis response is a *failed
//! analysis*: the bead is re-queued once, and a second failed analysis defaults
//! to (b) with the note `analysis dispatch produced no decision`. No path
//! applies `human` without an `analysis:` note — the note is written first and
//! the label only on its success, so a backend that cannot record notes cannot
//! produce a bare `human`.
//!
//! The prompt machinery is Unravel's (prompt → agent → parse → atomic child
//! creation via [`crate::bead_store::BeadStore::split_bead`]). Like Unravel,
//! this strand does not claim the bead it analyzes, so two concurrent workers
//! can in principle both dispatch; the Phase 19.4 generation audit and Mend's
//! orphaned-split-child sweep are what bound that duplication.
//!
//! Depends on: `bead_store`, `config`, `telemetry`, `types`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bead_store::{awaiting_analysis_dispatch, BeadStore, NewChild};
use crate::config::AnalyzeConfig;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{Bead, BeadId, StrandError, StrandResult};

/// Label marking a rung-4 child bead as the re-scope the analysis produced.
pub const RESCOPED_LABEL: &str = "escalation:rescoped";

/// Label marking the child bead of an escalation split (shared with Mitosis).
pub const SPLIT_CHILD_LABEL: &str = "split-child";

/// Rung this strand occupies on the Phase 19 escalation ladder.
pub const RUNG: u32 = 4;

/// The note recorded when the dispatch produced no decision at all, after the
/// single re-queue the ladder allows.
pub const NO_DECISION_ANALYSIS: &str = "analysis dispatch produced no decision";

// ─── Analysis agent ─────────────────────────────────────────────────────────

/// Abstraction for agent invocation used by the rung-4 analysis dispatch.
///
/// Production implementations wrap the dispatcher; tests use mocks.
#[async_trait::async_trait]
pub trait AnalysisAgent: Send + Sync {
    /// Invoke an agent with the given prompt, returning its raw text response.
    async fn analyze(&self, prompt: &str, workspace: &Path) -> Result<String>;
}

// ─── Decision (parsed from the agent response) ──────────────────────────────

/// The single conclusion an analysis dispatch is allowed to reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalysisDecision {
    /// (a) the work is re-scoped into one child bead.
    Rescope { title: String, body: String },
    /// (b) the plan does not answer how to proceed; a human must.
    Human { analysis: String },
}

/// Wire shape of the agent response, before validation.
#[derive(Debug, Deserialize)]
struct AnalysisResponse {
    decision: String,
    #[serde(default)]
    child: Option<RescopeChild>,
    #[serde(default)]
    analysis: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RescopeChild {
    title: String,
    #[serde(default)]
    body: Option<String>,
}

impl AnalysisDecision {
    /// Validate a parsed response into exactly one decision.
    ///
    /// Anything else — a bare `human` with no analysis, a re-scope with no
    /// usable child, or an unknown decision — is rejected: those are the
    /// "other outcomes" the handler must refuse rather than act on.
    fn from_response(response: AnalysisResponse) -> Option<Self> {
        match response.decision.as_str() {
            "rescope" => {
                let child = response.child?;
                let title = child.title.trim();
                let body = child.body.unwrap_or_default();
                let body = body.trim();
                if title.is_empty() || body.is_empty() {
                    return None;
                }
                Some(AnalysisDecision::Rescope {
                    title: title.to_string(),
                    body: body.to_string(),
                })
            }
            "human" => {
                let analysis = response.analysis?;
                let analysis = analysis.trim();
                if analysis.is_empty() {
                    // A bare `human` — the exact outcome the ladder forbids.
                    return None;
                }
                Some(AnalysisDecision::Human {
                    analysis: analysis.to_string(),
                })
            }
            _ => None,
        }
    }
}

/// Parse an agent response into exactly one [`AnalysisDecision`].
///
/// The response contract is a single JSON object, direct or inside a markdown
/// fence. Every JSON object the response contains is tried; the parse succeeds
/// only when exactly one of them names a valid decision. Two valid decisions
/// (or none) mean the agent did not return *exactly one* outcome, which the
/// handler treats as a failed analysis rather than guessing.
pub fn parse_analysis_response(response: &str) -> Result<AnalysisDecision> {
    let mut decisions = Vec::new();
    for candidate in json_objects(response) {
        let Ok(parsed) = serde_json::from_str::<AnalysisResponse>(&candidate) else {
            continue;
        };
        if let Some(decision) = AnalysisDecision::from_response(parsed) {
            decisions.push(decision);
        }
    }

    match decisions.len() {
        1 => Ok(decisions.remove(0)),
        0 => anyhow::bail!("agent response contained no valid analysis decision"),
        _ => anyhow::bail!(
            "agent response contained {} valid analysis decisions; exactly one is required",
            decisions.len()
        ),
    }
}

/// Extract the JSON objects from a response: the whole trimmed text, every
/// fenced code block, and every brace-balanced `{ ... }` span.
fn json_objects(response: &str) -> Vec<String> {
    let trimmed = response.trim();
    let mut candidates = vec![trimmed.to_string()];
    candidates.extend(fenced_blocks(trimmed));
    candidates.extend(braced_spans(trimmed));
    candidates
}

/// The contents of every ```-fenced block in the text.
fn fenced_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        let after_marker = &rest[start + 3..];
        // Skip an optional language tag on the opening line.
        let content_start = match after_marker.find('\n') {
            Some(line_end) => start + 3 + line_end + 1,
            None => break,
        };
        let Some(relative_end) = rest[content_start..].find("```") else {
            break;
        };
        blocks.push(
            rest[content_start..content_start + relative_end]
                .trim()
                .to_string(),
        );
        rest = &rest[content_start + relative_end + 3..];
    }
    blocks
}

/// Every brace-balanced `{ ... }` span, outermost first.
fn braced_spans(text: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let bytes = text.as_bytes();
    let mut depth: usize = 0;
    let mut start: Option<usize> = None;
    let mut in_string = false;
    let mut escaped = false;

    for (index, byte) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    start = Some(index);
                }
                depth += 1;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(from) = start.take() {
                        spans.push(text[from..=index].to_string());
                    }
                }
            }
            _ => {}
        }
    }
    spans
}

// ─── Persistent state ───────────────────────────────────────────────────────

/// Persisted per-workspace state: how many analysis dispatches each bead has
/// survived without producing a decision.
///
/// The ladder allows one re-queue after a failed analysis; this is what makes
/// that "once" survive a worker restart.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalyzeState {
    /// Map of bead ID -> failed analysis dispatch count.
    pub failed_analyses: HashMap<String, u32>,
}

impl AnalyzeState {
    /// Load state from disk, returning default if the file doesn't exist.
    fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(data) => serde_json::from_str(&data)
                .with_context(|| format!("failed to parse analyze state: {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => {
                Err(e).with_context(|| format!("failed to read analyze state: {}", path.display()))
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
            serde_json::to_string_pretty(self).context("failed to serialize analyze state")?;
        std::fs::write(path, data)
            .with_context(|| format!("failed to write analyze state: {}", path.display()))
    }

    /// Record a dispatch that produced no decision, returning how many this
    /// bead has now seen.
    fn record_failed_analysis(&mut self, bead_id: &BeadId) -> u32 {
        let count = self.failed_analyses.entry(bead_id.to_string()).or_insert(0);
        *count += 1;
        *count
    }

    fn clear(&mut self, bead_id: &BeadId) {
        self.failed_analyses.remove(bead_id.as_ref());
    }

    /// Whether this bead has already had its one allowed re-queue.
    fn has_requeued(&self, bead_id: &BeadId) -> bool {
        self.failed_analyses
            .get(bead_id.as_ref())
            .map(|count| *count >= 1)
            .unwrap_or(false)
    }
}

// ─── AnalyzeStrand ──────────────────────────────────────────────────────────

/// The Analyze strand — the rung-4 plan-grounded analysis dispatch.
pub struct AnalyzeStrand {
    config: AnalyzeConfig,
    workspace: PathBuf,
    state_dir: PathBuf,
    agent: Box<dyn AnalysisAgent>,
    telemetry: Telemetry,
}

/// What one bead's analysis dispatch concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeadOutcome {
    /// A re-scoped child was created; the waterfall should re-select.
    Rescoped,
    /// The bead was labelled `human` with an `analysis:` note.
    Human,
    /// No decision yet — the bead stays queued (its one re-queue or an
    /// infrastructure failure left it for a later cycle).
    Requeued,
}

impl AnalyzeStrand {
    /// Create a new AnalyzeStrand.
    ///
    /// `state_dir` is the base directory for analyze state files (e.g.,
    /// `~/.needle/state/analyze/`).
    pub fn new(
        config: AnalyzeConfig,
        workspace: PathBuf,
        state_dir: PathBuf,
        agent: Box<dyn AnalysisAgent>,
        telemetry: Telemetry,
    ) -> Self {
        AnalyzeStrand {
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

    /// Beads whose round-3 quarantine has expired and that no other rung owns.
    fn select_candidates(beads: &[Bead], now: chrono::DateTime<chrono::Utc>) -> Vec<Bead> {
        let mut candidates: Vec<Bead> = beads
            .iter()
            .filter(|bead| {
                awaiting_analysis_dispatch(bead, now)
                    // A human-labeled bead is rung 5's already; Unravel owns it.
                    && !bead.labels.iter().any(|label| label == "human")
            })
            .cloned()
            .collect();
        // Deterministic order (Principle 1): worst priority first, then oldest,
        // then id — the same shape as Pluck's sort key.
        candidates.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then(a.created_at.cmp(&b.created_at))
                .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
        });
        candidates
    }

    /// Build the analysis prompt for a rung-4 bead.
    ///
    /// Uses `config.prompt_template` when set; otherwise the built-in
    /// template. Template variables: `{id}`, `{title}`, `{body}`, `{labels}`,
    /// `{failure_trail}`, `{plan}`.
    fn build_prompt(&self, bead: &Bead, failure_trail: &str, plan: &str) -> String {
        let body = bead.body.as_deref().unwrap_or("(no description)");
        let labels = bead.labels.join(", ");

        if let Some(template) = &self.config.prompt_template {
            return template
                .replace("{id}", bead.id.as_ref())
                .replace("{title}", &bead.title)
                .replace("{body}", body)
                .replace("{labels}", &labels)
                .replace("{failure_trail}", failure_trail)
                .replace("{plan}", plan);
        }

        format!(
            "## Bead at escalation rung 4 (plan-grounded analysis)\n\n\
             **ID:** {id}\n\
             **Title:** {title}\n\
             **Labels:** {labels}\n\
             **Description:**\n\
             {body}\n\n\
             ## Failure trail\n\n\
             {failure_trail}\n\n\
             ## Workspace plan (docs/plan/plan.md)\n\n\
             {plan}\n\n\
             ## Task\n\n\
             This bead has failed repeatedly, exhausted its quarantine rounds, and \
             must now be settled one way. Read the plan and the failure trail and \
             decide between exactly two outcomes:\n\n\
             (a) **re-scope** — the goal is still right but the bead is mis-sized \
             or mis-shaped. Propose ONE child bead that a single agent could \
             complete autonomously: fully self-contained, no human decisions, no \
             dependency on NEEDLE's own internals.\n\
             (b) **human** — the plan does not answer how to proceed. State \
             precisely what the plan does not answer; a human will read it.\n\n\
             ## Response contract\n\n\
             Respond with EXACTLY ONE JSON object and nothing else — never both, \
             never neither:\n\n\
             {{\"decision\": \"rescope\", \"child\": {{\"title\": \"...\", \"body\": \"...\"}}}}\n\
             {{\"decision\": \"human\", \"analysis\": \"what the plan does not answer\"}}\n\n\
             A `human` response without a non-empty `analysis` string is invalid \
             and will be rejected.\n",
            id = bead.id,
            title = bead.title,
            labels = labels,
            body = body,
            failure_trail = failure_trail,
            plan = plan,
        )
    }

    /// The bead's full failure trail: every failure/quarantine label plus any
    /// operator notes the backend exposes.
    async fn build_failure_trail(&self, store: &dyn BeadStore, bead: &Bead) -> String {
        let mut trail = Vec::new();
        trail.push(format!("status: {:?}", bead.status));
        trail.push(format!(
            "assignee: {}",
            bead.assignee.as_deref().unwrap_or("(none)")
        ));
        let failure_labels: Vec<&str> = bead
            .labels
            .iter()
            .map(String::as_str)
            .filter(|label| {
                label.starts_with("failure-count:")
                    || label.starts_with("quarantine")
                    || *label == "cycling"
                    || *label == "retry-count"
            })
            .collect();
        if failure_labels.is_empty() {
            trail.push("failure labels: (none recorded)".to_string());
        } else {
            trail.push(format!("failure labels: {}", failure_labels.join(", ")));
        }

        match store.notes(&bead.id).await {
            Ok(Some(notes)) if !notes.trim().is_empty() => {
                trail.push(format!("recorded notes:\n{}", notes.trim()));
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(bead_id = %bead.id, error = %e, "could not read bead notes");
            }
        }

        trail.join("\n")
    }

    /// Load the workspace plan, or say plainly that there is none.
    fn load_plan(&self) -> String {
        let path = self.workspace.join(&self.config.plan_path);
        match std::fs::read_to_string(&path) {
            Ok(plan) if plan.trim().is_empty() => format!(
                "(the plan document at {} is empty)",
                self.config.plan_path.display()
            ),
            Ok(plan) => plan,
            Err(e) => format!(
                "(no plan document could be read at {}: {e}; there is no plan text to \
                 ground a re-scope in)",
                self.config.plan_path.display()
            ),
        }
    }

    /// Append the `analysis:` note to the bead's body.
    ///
    /// `update_description` replaces the whole description, so the current body
    /// is re-read from the store rather than reused from the in-memory copy —
    /// writing back a stale body would revert whatever landed since selection
    /// (the same rule the Phase 19.4 fold follows).
    async fn append_analysis_note(
        &self,
        store: &dyn BeadStore,
        bead_id: &BeadId,
        analysis: &str,
    ) -> Result<()> {
        let fresh = store
            .show(bead_id)
            .await
            .with_context(|| format!("failed to re-read {bead_id} before recording analysis"))?;
        let mut body = fresh.body.unwrap_or_default();
        if !body.trim().is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&format!(
            "analysis: {analysis}\n\n(rung-4 analysis dispatch, {})",
            Utc::now().to_rfc3339()
        ));
        store.update_description(bead_id, body.trim()).await
    }

    /// Apply outcome (b): `human` with an `analysis:` note.
    ///
    /// The note is written *before* the label. If the note cannot be recorded
    /// the label is not applied — a `human` label without its note is the
    /// defect ADR-022 names, and no code path here may produce one.
    async fn apply_human(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        analysis: &str,
    ) -> Result<BeadOutcome> {
        if let Err(e) = self.append_analysis_note(store, &bead.id, analysis).await {
            tracing::error!(
                bead_id = %bead.id,
                error = %e,
                "rung 4: could not record analysis note; human label NOT applied"
            );
            return Ok(BeadOutcome::Requeued);
        }

        if let Err(e) = store.add_label(&bead.id, "human").await {
            tracing::error!(
                bead_id = %bead.id,
                error = %e,
                "rung 4: analysis note recorded but human label failed; bead stays queued"
            );
            return Ok(BeadOutcome::Requeued);
        }

        self.clear_quarantine_labels(store, bead).await;

        tracing::info!(
            bead_id = %bead.id,
            analysis = %analysis,
            "rung 4 concluded the plan is silent: bead labelled human with analysis note"
        );
        let now = Utc::now();
        self.telemetry
            .emit(
                EventKind::BeadEscalated {
                    bead_id: bead.id.clone(),
                    rung: RUNG,
                },
                now,
            )
            .ok();
        self.telemetry
            .emit(
                EventKind::HumanRung {
                    bead_id: bead.id.clone(),
                    analysis: analysis.to_string(),
                },
                now,
            )
            .ok();

        Ok(BeadOutcome::Human)
    }

    /// Apply outcome (a): create the re-scoped child as a blocker of the
    /// original and leave the original open and dependency-blocked.
    async fn apply_rescope(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        title: &str,
        body: &str,
        analysis_context: &str,
    ) -> Result<BeadOutcome> {
        let child_title = format!("[Rung 4 re-scope] {title}");
        let child_body = format!(
            "## Re-scope of: {parent_title}\n\
             Original bead: {id}\n\n\
             {body}\n\n\
             ---\n\
             Created by the rung-4 analysis dispatch (Phase 19, ADR-022): the \
             parent exhausted its quarantine rounds and this child is the \
             re-scoped work. The parent stays open, blocked by this bead.",
            parent_title = bead.title,
            id = bead.id,
            body = body,
        );
        let child_labels = [SPLIT_CHILD_LABEL, RESCOPED_LABEL];

        // Atomic create + link (plan.md Phase 5.3, Race 3): a crash cannot
        // orphan the child without its dependency edge.
        let child_ids = store
            .split_bead(
                &bead.id,
                &[NewChild {
                    title: &child_title,
                    body: &child_body,
                    labels: &child_labels,
                }],
            )
            .await
            .with_context(|| format!("failed to create rung-4 re-scope child of {}", bead.id))?;

        // The note is rung 4's evidence. The structural outcome (child blocking
        // the parent) already holds, so a note failure warns instead of
        // re-queueing the bead for a second analysis and a second child.
        if let Err(e) = self
            .append_analysis_note(store, &bead.id, analysis_context)
            .await
        {
            tracing::warn!(
                bead_id = %bead.id,
                error = %e,
                "rung 4: re-scope child created but the analysis note could not be recorded"
            );
        }

        self.clear_quarantine_labels(store, bead).await;

        for child_id in &child_ids {
            tracing::info!(
                parent_id = %bead.id,
                child_id = %child_id,
                "rung 4 re-scoped the bead into a child; parent stays open and blocked"
            );
        }
        self.telemetry
            .emit(
                EventKind::BeadEscalated {
                    bead_id: bead.id.clone(),
                    rung: RUNG,
                },
                Utc::now(),
            )
            .ok();

        Ok(BeadOutcome::Rescoped)
    }

    /// Drop the quarantine labels a concluded rung-4 outcome supersedes.
    ///
    /// The round-3 label is the rung-4 trigger, so leaving it behind would
    /// re-analyze the same bead every cycle; and a bead that is now human- or
    /// dependency-held is no longer quarantined, so the quarantine counters
    /// must not keep counting it.
    async fn clear_quarantine_labels(&self, store: &dyn BeadStore, bead: &Bead) {
        let labels = match store.labels(&bead.id).await {
            Ok(labels) => labels,
            Err(e) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "rung 4: could not read labels to clear quarantine state"
                );
                return;
            }
        };
        for label in labels.iter().filter(|label| is_quarantine_label(label)) {
            if let Err(e) = store.remove_label(&bead.id, label).await {
                tracing::warn!(
                    bead_id = %bead.id,
                    label,
                    error = %e,
                    "rung 4: failed to remove superseded quarantine label"
                );
            }
        }
    }

    /// Run the analysis dispatch for one bead and apply its outcome.
    async fn analyze_bead(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        state: &mut AnalyzeState,
    ) -> BeadOutcome {
        let failure_trail = self.build_failure_trail(store, bead).await;
        let plan = self.load_plan();
        let prompt = self.build_prompt(bead, &failure_trail, &plan);

        let decision = self
            .agent
            .analyze(&prompt, &self.workspace)
            .await
            .and_then(|response| {
                parse_analysis_response(&response)
                    .with_context(|| format!("failed to parse analysis response for {}", bead.id))
            });

        let decision = match decision {
            Ok(decision) => decision,
            Err(error) => {
                // Failed analysis: re-queue once, then settle on (b) with the
                // default note so the bead cannot cycle here forever.
                if state.has_requeued(&bead.id) {
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %error,
                        "rung 4: re-queued analysis produced no decision either; defaulting to human"
                    );
                    let outcome = self
                        .apply_human(store, bead, NO_DECISION_ANALYSIS)
                        .await
                        .unwrap_or(BeadOutcome::Requeued);
                    if outcome == BeadOutcome::Human {
                        state.clear(&bead.id);
                    }
                    return outcome;
                }
                let attempts = state.record_failed_analysis(&bead.id);
                tracing::warn!(
                    bead_id = %bead.id,
                    attempts,
                    error = %error,
                    "rung 4: analysis produced no decision; re-queueing the bead once"
                );
                return BeadOutcome::Requeued;
            }
        };

        let outcome = match decision {
            AnalysisDecision::Rescope { title, body } => {
                let note =
                    format!("re-scoped into child bead ({title}); parent stays open and blocked");
                self.apply_rescope(store, bead, &title, &body, &note).await
            }
            AnalysisDecision::Human { analysis } => self.apply_human(store, bead, &analysis).await,
        };

        match outcome {
            Ok(outcome) => {
                if matches!(outcome, BeadOutcome::Rescoped | BeadOutcome::Human) {
                    state.clear(&bead.id);
                }
                outcome
            }
            // A valid decision the store refused to apply is infrastructure,
            // not a failed analysis: leave the bead queued without spending its
            // re-queue, and without deciding `human` on a store's behalf.
            Err(error) => {
                tracing::error!(
                    bead_id = %bead.id,
                    error = %error,
                    "rung 4: could not apply the analysis decision; bead stays queued"
                );
                BeadOutcome::Requeued
            }
        }
    }
}

/// Whether `label` is one of the quarantine labels a concluded rung-4 outcome
/// supersedes.
fn is_quarantine_label(label: &str) -> bool {
    label == "quarantined"
        || label.starts_with("quarantine-until:")
        || label.starts_with("quarantine-round:")
        || label.starts_with("quarantine:")
}

#[async_trait::async_trait]
impl super::Strand for AnalyzeStrand {
    fn name(&self) -> &str {
        "analyze"
    }

    async fn evaluate(&self, store: &dyn BeadStore, _exclusions: &HashSet<BeadId>) -> StrandResult {
        // Guard: disabled.
        if !self.config.enabled {
            tracing::debug!("analyze strand disabled");
            return StrandResult::NoWork;
        }

        // No home store means a roam-only worker: this workspace has no rung-4
        // beads of its own to settle.
        if !store.has_valid_store() {
            tracing::info!(
                "Home workspace has no .beads/ directory — skipping Analyze strand \
                 (expected for roam-only workers)"
            );
            return StrandResult::Skipped {
                reason: "no_home_store".to_string(),
            };
        }

        let state_path = self.state_file_path();
        let mut state = match AnalyzeState::load(&state_path) {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load analyze state, using defaults");
                AnalyzeState::default()
            }
        };

        let all_beads = match store.list_all().await {
            Ok(beads) => beads,
            Err(e) => {
                tracing::warn!(error = %e, "analyze strand: failed to list beads");
                return StrandResult::Error(StrandError::StoreError(e));
            }
        };

        let now = Utc::now();
        let candidates = Self::select_candidates(&all_beads, now);
        if candidates.is_empty() {
            tracing::debug!("analyze strand: no beads waiting at rung 4");
            return StrandResult::NoWork;
        }

        tracing::info!(
            beads = candidates.len(),
            "analyze strand: beads awaiting the rung-4 analysis dispatch"
        );

        let mut any_rescoped = false;
        for bead in candidates
            .iter()
            .take(self.config.max_beads_per_run as usize)
        {
            // The expiry event fires once per bead: a concluded outcome removes
            // the round-3 label that produced it.
            self.telemetry
                .emit(
                    EventKind::QuarantineExpired {
                        bead_id: bead.id.clone(),
                    },
                    now,
                )
                .ok();

            match self.analyze_bead(store, bead, &mut state).await {
                BeadOutcome::Rescoped => any_rescoped = true,
                BeadOutcome::Human | BeadOutcome::Requeued => {}
            }
        }

        if let Err(e) = state.save(&state_path) {
            tracing::warn!(error = %e, "analyze strand: failed to save state");
        }

        if any_rescoped {
            StrandResult::WorkCreated
        } else {
            StrandResult::NoWork
        }
    }
}

// ─── Production agent ────────────────────────────────────────────────────────

/// Production `AnalysisAgent` that invokes the configured AI agent via subprocess.
///
/// Mirrors Unravel's CLI agent: the agent binary runs with `--print` so the
/// response arrives as plain text on stdout without tool-use side effects, and
/// the prompt is fed from a temp file.
pub struct CliAnalysisAgent {
    /// Agent binary name or path (e.g., `"claude"`).
    agent_cmd: String,
}

impl CliAnalysisAgent {
    /// Create a new `CliAnalysisAgent`.
    ///
    /// `agent_cmd` is the binary used for analysis (typically taken from
    /// `config.agent.default`).
    pub fn new(agent_cmd: String) -> Self {
        CliAnalysisAgent { agent_cmd }
    }
}

#[async_trait::async_trait]
impl AnalysisAgent for CliAnalysisAgent {
    async fn analyze(&self, prompt: &str, workspace: &Path) -> Result<String> {
        // Write the prompt to a temp file.
        let tmp_dir = std::env::temp_dir().join("needle");
        std::fs::create_dir_all(&tmp_dir)
            .context("failed to create needle temp dir for analyze")?;
        let tmp_file = tmp_dir.join(format!("analyze-{}.md", std::process::id()));
        std::fs::write(&tmp_file, prompt).context("failed to write analyze prompt to temp file")?;

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
            .with_context(|| format!("failed to spawn analyze agent: {}", self.agent_cmd))?;

        // Always clean up the temp file.
        let _ = std::fs::remove_file(&tmp_file);

        if !output.status.success() {
            anyhow::bail!(
                "analyze agent exited with code {}",
                output.status.code().unwrap_or(-1)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

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

    // ── Mock agents ─────────────────────────────────────────────────────

    /// Returns scripted responses in order; the last entry repeats forever.
    ///
    /// `Clone` shares the call counter, so a clone kept outside a moved agent
    /// still observes how often the agent was dispatched.
    #[derive(Clone)]
    struct MockAgent {
        responses: std::sync::Arc<Mutex<Vec<String>>>,
        calls: std::sync::Arc<Mutex<usize>>,
    }

    impl MockAgent {
        fn new(response: &str) -> Self {
            MockAgent::scripted(vec![response])
        }

        fn scripted(responses: Vec<&str>) -> Self {
            MockAgent {
                responses: std::sync::Arc::new(Mutex::new(
                    responses.into_iter().map(str::to_string).collect(),
                )),
                calls: std::sync::Arc::new(Mutex::new(0)),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl AnalysisAgent for MockAgent {
        async fn analyze(&self, _prompt: &str, _workspace: &Path) -> Result<String> {
            *self.calls.lock().unwrap() += 1;
            let mut responses = self.responses.lock().unwrap();
            if responses.len() > 1 {
                Ok(responses.remove(0))
            } else {
                Ok(responses[0].clone())
            }
        }
    }

    struct FailingAgent;

    #[async_trait::async_trait]
    impl AnalysisAgent for FailingAgent {
        async fn analyze(&self, _prompt: &str, _workspace: &Path) -> Result<String> {
            anyhow::bail!("agent dispatch failed")
        }
    }

    // ── Mock BeadStore ──────────────────────────────────────────────────

    struct MockStore {
        beads: Mutex<Vec<Bead>>,
        /// (title, body, labels) of every created bead.
        created: Mutex<Vec<(String, String, Vec<String>)>>,
        /// (child, parent) dependency edges added.
        deps_added: Mutex<Vec<(String, String)>>,
        /// Labels added per bead.
        labels_added: Mutex<Vec<(String, String)>>,
        /// Labels removed per bead.
        labels_removed: Mutex<Vec<(String, String)>>,
        /// Description writes per bead.
        descriptions: Mutex<Vec<(String, String)>>,
        /// When set, `update_description` fails, emulating a backend that
        /// cannot update descriptions (bead-rs `update`).
        fail_update_description: bool,
        /// When set, `split_bead` fails.
        fail_split: bool,
    }

    impl MockStore {
        fn new(beads: Vec<Bead>) -> Self {
            MockStore {
                beads: Mutex::new(beads),
                created: Mutex::new(Vec::new()),
                deps_added: Mutex::new(Vec::new()),
                labels_added: Mutex::new(Vec::new()),
                labels_removed: Mutex::new(Vec::new()),
                descriptions: Mutex::new(Vec::new()),
                fail_update_description: false,
                fail_split: false,
            }
        }

        fn created_beads(&self) -> Vec<(String, String, Vec<String>)> {
            self.created.lock().unwrap().clone()
        }

        fn deps(&self) -> Vec<(String, String)> {
            self.deps_added.lock().unwrap().clone()
        }

        fn added_labels(&self) -> Vec<(String, String)> {
            self.labels_added.lock().unwrap().clone()
        }

        fn removed_labels(&self) -> Vec<(String, String)> {
            self.labels_removed.lock().unwrap().clone()
        }

        fn descriptions(&self) -> Vec<(String, String)> {
            self.descriptions.lock().unwrap().clone()
        }

        #[allow(dead_code)]
        fn set_bead_labels(&self, id: &str, labels: &[&str]) {
            self.beads
                .lock()
                .unwrap()
                .iter_mut()
                .filter(|bead| bead.id.as_ref() == id)
                .for_each(|bead| {
                    bead.labels = labels.iter().map(|s| s.to_string()).collect();
                });
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for MockStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().unwrap().clone())
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().unwrap().clone())
        }
        async fn show(&self, id: &BeadId) -> Result<Bead> {
            self.beads
                .lock()
                .unwrap()
                .iter()
                .find(|bead| bead.id == *id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("bead {id} not found"))
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "mock".to_string(),
            })
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
        async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
            Ok(self
                .beads
                .lock()
                .unwrap()
                .iter()
                .find(|bead| bead.id == *id)
                .map(|bead| bead.labels.clone())
                .unwrap_or_default())
        }
        async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
            self.labels_added
                .lock()
                .unwrap()
                .push((id.to_string(), label.to_string()));
            self.beads
                .lock()
                .unwrap()
                .iter_mut()
                .filter(|bead| bead.id == *id)
                .for_each(|bead| bead.labels.push(label.to_string()));
            Ok(())
        }
        async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
            self.labels_removed
                .lock()
                .unwrap()
                .push((id.to_string(), label.to_string()));
            self.beads
                .lock()
                .unwrap()
                .iter_mut()
                .filter(|bead| bead.id == *id)
                .for_each(|bead| bead.labels.retain(|l| l != label));
            Ok(())
        }
        async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
            let mut created = self.created.lock().unwrap();
            created.push((
                title.to_string(),
                body.to_string(),
                labels.iter().map(|s| s.to_string()).collect(),
            ));
            let id = format!("analyze-child-{}", created.len());
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
        async fn update_description(&self, id: &BeadId, description: &str) -> Result<()> {
            if self.fail_update_description {
                anyhow::bail!("configured bead backend does not implement update_description");
            }
            self.descriptions
                .lock()
                .unwrap()
                .push((id.to_string(), description.to_string()));
            self.beads
                .lock()
                .unwrap()
                .iter_mut()
                .filter(|bead| bead.id == *id)
                .for_each(|bead| bead.body = Some(description.to_string()));
            Ok(())
        }
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
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
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "claim_auto not supported in mock".to_string(),
            })
        }

        async fn split_bead(
            &self,
            _parent_id: &BeadId,
            children: &[NewChild<'_>],
        ) -> Result<Vec<BeadId>> {
            if self.fail_split {
                anyhow::bail!("mock store refused the split");
            }
            let mut created = Vec::with_capacity(children.len());
            for child in children {
                let child_id = self
                    .create_bead(child.title, child.body, child.labels)
                    .await?;
                self.add_dependency(&child_id, _parent_id).await?;
                created.push(child_id);
            }
            Ok(created)
        }

        fn has_valid_store(&self) -> bool {
            true
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn make_bead(id: &str, labels: &[&str]) -> Bead {
        let dt = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        Bead {
            id: BeadId::from(id.to_string()),
            title: format!("Bead {id}"),
            body: Some(format!("Description for {id}")),
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            workspace: PathBuf::from("/tmp/test"),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: dt,
            updated_at: dt,
        }
    }

    /// A bead sitting at rung 4: quarantine round 3, window long past.
    fn make_rung4_bead(id: &str) -> Bead {
        make_bead(
            id,
            &[
                "quarantined",
                "quarantine-round:3",
                "quarantine-until:2026-01-01T02:00:00+00:00",
                "failure-count:7",
                "cycling",
            ],
        )
    }

    fn make_strand(dir: &std::path::Path, agent: Box<dyn AnalysisAgent>) -> AnalyzeStrand {
        AnalyzeStrand::new(
            AnalyzeConfig {
                enabled: true,
                ..AnalyzeConfig::default()
            },
            PathBuf::from("/tmp/test-workspace"),
            dir.to_path_buf(),
            agent,
            Telemetry::new("test".to_string()),
        )
    }

    use super::super::Strand;

    const RESCOPE_RESPONSE: &str = r#"{"decision": "rescope", "child": {"title": "Do the smaller thing", "body": "Steps for the smaller thing"}}"#;
    const HUMAN_RESPONSE: &str = r#"{"decision": "human", "analysis": "the plan has no acceptance criteria for this subsystem"}"#;

    // ── Parser tests ────────────────────────────────────────────────────

    #[test]
    fn parse_rescope_decision() {
        let decision = parse_analysis_response(RESCOPE_RESPONSE).unwrap();
        assert_eq!(
            decision,
            AnalysisDecision::Rescope {
                title: "Do the smaller thing".to_string(),
                body: "Steps for the smaller thing".to_string(),
            }
        );
    }

    #[test]
    fn parse_human_decision() {
        let decision = parse_analysis_response(HUMAN_RESPONSE).unwrap();
        assert_eq!(
            decision,
            AnalysisDecision::Human {
                analysis: "the plan has no acceptance criteria for this subsystem".to_string(),
            }
        );
    }

    #[test]
    fn parse_accepts_fenced_and_prose_wrapped_json() {
        let fenced = format!("Here is my decision:\n\n```json\n{HUMAN_RESPONSE}\n```\n");
        assert!(parse_analysis_response(&fenced).is_ok());

        let prose = format!("After reading the plan I conclude:\n{RESCOPE_RESPONSE}\nThank you.");
        assert!(parse_analysis_response(&prose).is_ok());
    }

    #[test]
    fn parse_rejects_bare_human_without_analysis() {
        // The outcome the ladder forbids: `human` with nothing justifying it.
        let bare = r#"{"decision": "human"}"#;
        assert!(parse_analysis_response(bare).is_err());

        let empty = r#"{"decision": "human", "analysis": "   "}"#;
        assert!(parse_analysis_response(empty).is_err());
    }

    #[test]
    fn parse_rejects_rescope_without_a_usable_child() {
        assert!(parse_analysis_response(r#"{"decision": "rescope"}"#).is_err());
        assert!(parse_analysis_response(
            r#"{"decision": "rescope", "child": {"title": "", "body": "x"}}"#
        )
        .is_err());
        assert!(parse_analysis_response(
            r#"{"decision": "rescope", "child": {"title": "t", "body": ""}}"#
        )
        .is_err());
    }

    #[test]
    fn parse_rejects_unknown_decision_garbage_and_silence() {
        assert!(parse_analysis_response(r#"{"decision": "retry"}"#).is_err());
        assert!(parse_analysis_response("I cannot decide.").is_err());
        assert!(parse_analysis_response("").is_err());
        assert!(parse_analysis_response("[]").is_err());
    }

    #[test]
    fn parse_rejects_two_decisions() {
        let both = format!("{RESCOPE_RESPONSE}\n{HUMAN_RESPONSE}");
        assert!(parse_analysis_response(&both).is_err());
    }

    #[test]
    fn parse_handles_braces_inside_strings() {
        let tricky =
            r#"{"decision": "human", "analysis": "the plan's {id} placeholder is unresolved"}"#;
        let decision = parse_analysis_response(tricky).unwrap();
        assert!(matches!(decision, AnalysisDecision::Human { .. }));
    }

    // ── Strand behaviour with a mocked agent ────────────────────────────

    #[tokio::test]
    async fn rescope_creates_blocking_child_and_leaves_parent_open() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(vec![make_rung4_bead("bead-a")]);
        let strand = make_strand(dir.path(), Box::new(MockAgent::new(RESCOPE_RESPONSE)));

        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::WorkCreated));

        // (a) the child exists with the escalation labels…
        let created = store.created_beads();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].0, "[Rung 4 re-scope] Do the smaller thing");
        assert!(created[0].2.contains(&"split-child".to_string()));
        assert!(created[0].2.contains(&"escalation:rescoped".to_string()));

        // …and blocks the original.
        assert_eq!(
            store.deps(),
            vec![("analyze-child-1".to_string(), "bead-a".to_string())]
        );

        // The parent stays open (never a status change) and keeps its round-3
        // trail out of the quarantine counters.
        let parent = store.show(&BeadId::from("bead-a")).await.unwrap();
        assert_eq!(parent.status, BeadStatus::Open);
        assert!(store
            .removed_labels()
            .contains(&("bead-a".to_string(), "quarantine-round:3".to_string())));

        // Rung 4's evidence is on the bead.
        let descriptions = store.descriptions();
        assert_eq!(descriptions.len(), 1);
        assert!(descriptions[0]
            .1
            .contains("analysis: re-scoped into child bead"));
    }

    #[tokio::test]
    async fn human_applies_label_with_analysis_note() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(vec![make_rung4_bead("bead-b")]);
        let strand = make_strand(dir.path(), Box::new(MockAgent::new(HUMAN_RESPONSE)));

        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));

        // No child was created…
        assert!(store.created_beads().is_empty());
        assert!(store.deps().is_empty());

        // …the human label was applied…
        assert!(store
            .added_labels()
            .contains(&("bead-b".to_string(), "human".to_string())));

        // …with the analysis note written before it…
        let descriptions = store.descriptions();
        assert_eq!(descriptions.len(), 1);
        assert!(descriptions[0]
            .1
            .contains("analysis: the plan has no acceptance criteria"));

        // …and the quarantine trail was cleared.
        assert!(store
            .removed_labels()
            .contains(&("bead-b".to_string(), "quarantine-round:3".to_string())));
    }

    #[tokio::test]
    async fn undecided_response_requeues_once_then_defaults_to_human() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(vec![make_rung4_bead("bead-c")]);
        // Neither outcome, twice: the script serves the first response on the
        // first dispatch and repeats it on the second.
        let agent = Box::new(MockAgent::new("I read the plan but cannot say."));
        let strand = make_strand(dir.path(), agent);

        // First dispatch: failed analysis, bead re-queued, nothing decided.
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert!(store.added_labels().is_empty());
        assert!(store.descriptions().is_empty());
        assert!(store.created_beads().is_empty());

        // Second dispatch: defaults to (b) with the default note.
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert!(store
            .added_labels()
            .contains(&("bead-c".to_string(), "human".to_string())));
        let descriptions = store.descriptions();
        assert_eq!(descriptions.len(), 1);
        assert!(descriptions[0]
            .1
            .contains("analysis: analysis dispatch produced no decision"));
    }

    #[tokio::test]
    async fn valid_decision_after_a_requeue_is_still_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(vec![make_rung4_bead("bead-d")]);
        let agent = Box::new(MockAgent::scripted(vec!["(no json)", RESCOPE_RESPONSE]));
        let strand = make_strand(dir.path(), agent);

        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));

        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::WorkCreated));
        assert_eq!(store.created_beads().len(), 1);
        assert!(!store.added_labels().iter().any(|(_, l)| l == "human"));
    }

    #[tokio::test]
    async fn dispatch_failure_never_applies_human_on_the_first_try() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(vec![make_rung4_bead("bead-e")]);
        let strand = make_strand(dir.path(), Box::new(FailingAgent));

        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert!(store.added_labels().is_empty());
        assert!(store.descriptions().is_empty());
    }

    #[tokio::test]
    async fn failing_dispatch_twice_still_lands_on_human_with_the_note() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(vec![make_rung4_bead("bead-f")]);
        let strand = make_strand(dir.path(), Box::new(FailingAgent));

        let _ = strand.evaluate(&store, &HashSet::new()).await;
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert!(store
            .added_labels()
            .contains(&("bead-f".to_string(), "human".to_string())));
        assert!(store.descriptions()[0]
            .1
            .contains("analysis: analysis dispatch produced no decision"));
    }

    #[tokio::test]
    async fn no_path_applies_human_without_the_analysis_note() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MockStore::new(vec![make_rung4_bead("bead-g")]);
        store.fail_update_description = true;
        let strand = make_strand(dir.path(), Box::new(MockAgent::new(HUMAN_RESPONSE)));

        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));

        // The note could not be recorded, so the label must not exist.
        assert!(!store
            .added_labels()
            .iter()
            .any(|(_, label)| label == "human"));
        assert!(store.descriptions().is_empty());
    }

    #[tokio::test]
    async fn bare_human_response_is_rejected_not_applied() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(vec![make_rung4_bead("bead-h")]);
        let strand = make_strand(
            dir.path(),
            Box::new(MockAgent::new(r#"{"decision": "human"}"#)),
        );

        // A bare `human` is not a decision: it is a failed analysis.
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert!(!store
            .added_labels()
            .iter()
            .any(|(_, label)| label == "human"));

        // The re-queue is spent, so the next failure settles on (b) — with the
        // note, never without.
        let _ = strand.evaluate(&store, &HashSet::new()).await;
        assert!(store
            .added_labels()
            .contains(&("bead-h".to_string(), "human".to_string())));
        assert!(store.descriptions()[0]
            .1
            .contains("analysis: analysis dispatch produced no decision"));
    }

    #[tokio::test]
    async fn failed_rescope_application_leaves_bead_queued_without_human() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MockStore::new(vec![make_rung4_bead("bead-i")]);
        store.fail_split = true;
        let strand = make_strand(dir.path(), Box::new(MockAgent::new(RESCOPE_RESPONSE)));

        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert!(store.created_beads().is_empty());
        // A store failure is not a failed analysis: no `human` may be decided
        // on the store's behalf.
        assert!(!store
            .added_labels()
            .iter()
            .any(|(_, label)| label == "human"));
    }

    #[tokio::test]
    async fn only_expired_round3_beads_are_selected() {
        let now = Utc::now();
        let future = (now + chrono::Duration::hours(2)).to_rfc3339();
        let expired = (now - chrono::Duration::hours(2)).to_rfc3339();

        let round3_active = make_bead(
            "active",
            &[
                "quarantined",
                "quarantine-round:3",
                &format!("quarantine-until:{future}"),
            ],
        );
        let round1_expired = make_bead(
            "round1",
            &[
                "quarantined",
                "quarantine-round:1",
                &format!("quarantine-until:{expired}"),
            ],
        );
        let human_labeled = make_bead(
            "human",
            &[
                "quarantine-round:3",
                &format!("quarantine-until:{expired}"),
                "human",
            ],
        );
        let mut in_progress = make_bead(
            "working",
            &["quarantine-round:3", &format!("quarantine-until:{expired}")],
        );
        in_progress.status = BeadStatus::InProgress;
        let ready = make_rung4_bead("ready");

        let beads = vec![
            round3_active,
            round1_expired,
            human_labeled,
            in_progress,
            ready,
        ];
        let selected = AnalyzeStrand::select_candidates(&beads, now);
        assert_eq!(
            selected.iter().map(|b| b.id.as_ref()).collect::<Vec<_>>(),
            vec!["ready"]
        );
    }

    #[tokio::test]
    async fn disabled_strand_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(vec![make_rung4_bead("bead-j")]);
        let strand = AnalyzeStrand::new(
            AnalyzeConfig {
                enabled: false,
                ..AnalyzeConfig::default()
            },
            PathBuf::from("/tmp/test-workspace"),
            dir.path().to_path_buf(),
            Box::new(MockAgent::new(HUMAN_RESPONSE)),
            Telemetry::new("test".to_string()),
        );

        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert!(store.added_labels().is_empty());
    }

    #[tokio::test]
    async fn max_beads_per_run_is_respected() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(vec![make_rung4_bead("bead-1"), make_rung4_bead("bead-2")]);
        let strand = AnalyzeStrand::new(
            AnalyzeConfig {
                enabled: true,
                max_beads_per_run: 1,
                ..AnalyzeConfig::default()
            },
            PathBuf::from("/tmp/test-workspace"),
            dir.path().to_path_buf(),
            Box::new(MockAgent::new(HUMAN_RESPONSE)),
            Telemetry::new("test".to_string()),
        );

        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));

        // Exactly one bead was settled this run.
        let humaned = store
            .added_labels()
            .iter()
            .filter(|(_, label)| label == "human")
            .count();
        assert_eq!(humaned, 1);
    }

    #[test]
    fn prompt_carries_bead_trail_and_plan() {
        let dir = tempfile::tempdir().unwrap();
        let strand = make_strand(dir.path(), Box::new(MockAgent::new(HUMAN_RESPONSE)));
        let bead = make_rung4_bead("bead-k");
        let prompt = strand.build_prompt(&bead, "failure labels: failure-count:7", "# The Plan");

        assert!(prompt.contains("bead-k"));
        assert!(prompt.contains("Bead bead-k"));
        assert!(prompt.contains("failure-count:7"));
        assert!(prompt.contains("# The Plan"));
        assert!(prompt.contains("\"decision\": \"rescope\""));
        assert!(prompt.contains("\"decision\": \"human\""));
    }

    #[tokio::test]
    async fn two_concluded_beads_restart_the_waterfall() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(vec![make_rung4_bead("bead-1"), make_rung4_bead("bead-2")]);
        let strand = make_strand(dir.path(), Box::new(MockAgent::new(RESCOPE_RESPONSE)));

        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::WorkCreated));
        assert_eq!(store.created_beads().len(), 2);
        assert_eq!(store.deps().len(), 2);
    }

    #[test]
    fn quarantine_label_cleanup_covers_the_whole_scheme() {
        assert!(is_quarantine_label("quarantined"));
        assert!(is_quarantine_label("quarantine-round:3"));
        assert!(is_quarantine_label(
            "quarantine-until:2026-01-01T00:00:00+00:00"
        ));
        assert!(is_quarantine_label("quarantine:failure-count:7"));
        assert!(!is_quarantine_label("cycling"));
        assert!(!is_quarantine_label("human"));
        assert!(!is_quarantine_label("escalation:rescoped"));
    }

    #[tokio::test]
    async fn analyzed_beads_are_not_reanalyzed() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(vec![make_rung4_bead("bead-m")]);
        let agent = MockAgent::new(RESCOPE_RESPONSE);
        let agent_calls = agent.clone();
        let strand = make_strand(dir.path(), Box::new(agent));

        let _ = strand.evaluate(&store, &HashSet::new()).await;
        let created_after_first = store.created_beads().len();
        assert_eq!(created_after_first, 1);

        // The round-3 trigger label is gone, so a second pass re-selects
        // nothing and creates no second child.
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert_eq!(store.created_beads().len(), 1);
        // Two dispatches happened only if the bead was re-selected; it was not.
        assert_eq!(agent_calls.calls(), 1);
    }
}
