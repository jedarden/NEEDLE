//! ci-watch strand: file one P0 bead per distinct needle-ci red on main.
//!
//! Every poll interval the strand resolves the recent `origin/main` history,
//! asks for the newest *completed* CI verdict on those revisions, and — when
//! that verdict is red — files a P0 bead carrying the failing step, the
//! failing test ids, the first failing revision, that revision's parent's
//! status, and the workflow name. The failure fingerprint is the step plus
//! the sorted test ids: a later red with the same fingerprint appends a note
//! to the existing bead instead of filing a new one, and a green verdict
//! never touches any bead.
//!
//! **Verdict source.** needle-ci publishes its failure summary as a Forgejo
//! commit status (context `iad-ci/<template>`, description
//! `needle-ci Failed: <step>: <N> failed: <test ids>`). The strand reads the
//! status attached to the newest recently verdicted `origin/main` revision.
//! Looking behind an unverdicted tip matters because main can advance while a
//! long CI run is still executing. A missing status is retried on the next
//! poll, preserving the workflow's own test identifiers as the fingerprint
//! input.
//!
//! **Lifecycle contract.** The strand creates and notes; it never closes. A
//! bead it filed is closed by whoever lands the fix. When a fingerprint
//! recurs after its bead was already closed, the strand appends a note and
//! reopens the bead so the regression is claimable again.
//!
//! **State.** `<state>/ci_watch/<workspace-hash>.json` records the last poll
//! time, the last observed verdict (revision + status-record identity), and
//! the fingerprint → bead map. An identical re-observation is a no-op, so a
//! crashed or racing worker repolling the same status does nothing twice.
//! Bead-level deduplication is additionally enforced by the backend's
//! `--unique-ref` (`needle-ci-red:<fingerprint>`): two workers that both see
//! an unfiled fingerprint produce one bead and one race note.
//!
//! Depends on: `bead_store`, `build_status`, `config`, `telemetry`, `types`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bead_store::{BeadStore, DedupCreate};
use crate::build_status::{
    main_branch_sha, template_for_workspace, BuildStatus, CiStatusSource, CiWorkflowRun,
    ForgejoStatusSource,
};
use crate::config::CiWatchConfig;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{BeadId, BeadStatus, StrandResult};

/// Namespace half of the backend unique-ref used for fingerprint dedupe.
pub const UNIQUE_REF_NAMESPACE: &str = "needle-ci-red";

/// Labels every filed red bead carries.
///
/// `ci-red` is the label the build-status circuit breaker hardcodes as
/// claimable while main is red (needle-2724c8f2: a red gate must stop work,
/// not stop the repair); `fix-build` is the configured companion; the other
/// two mirror the post-push CI lifecycle beads.
const RED_LABELS: [&str; 4] = ["ci-red", "fix-build", "origin:ci", "priority:0"];

/// Priority of a filed red bead: P0, ahead of everything else in the queue.
const RED_PRIORITY: u8 = 0;

/// Telemetry phase stamped on every ci-watch event.
const PHASE: &str = "ci_watch";

/// Number of recent mainline revisions inspected for the newest completed
/// verdict. This comfortably covers the commits that can land during one
/// full needle-ci run without turning an unavailable status API into an
/// unbounded history scan.
const VERDICT_HISTORY_LIMIT: usize = 32;

// ─── Failure summary parsing ────────────────────────────────────────────────

/// The failure facts one red verdict carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailureSummary {
    /// CI step that failed (for example `verify` or
    /// `publish-failure-summary`).
    pub failing_step: String,
    /// Failing test ids, in the order the summary listed them.
    pub failing_test_ids: Vec<String>,
}

/// Split `<step>: <N> failed: <test, test, ...>` into step and test ids.
///
/// needle-ci's publish-failure-summary step writes this shape into the
/// Forgejo status description. The description is truncated to 140 chars by
/// the API, so the final test id may be cut mid-name; it is kept as-is rather
/// than dropped, keeping the fingerprint stable between the truncated status
/// and (later) the full summary. Returns `None` when the remainder does not
/// carry a test list — the step-only and fallback description shapes.
fn split_step_and_tests(rest: &str) -> Option<(String, Vec<String>)> {
    for (index, _) in rest.match_indices(": ") {
        let after = &rest[index + 2..];
        let Some(digits_end) = after.find(' ') else {
            continue;
        };
        if !after[..digits_end].bytes().all(|b| b.is_ascii_digit())
            || digits_end == 0
            || !after[digits_end + 1..].starts_with("failed:")
        {
            continue;
        }
        let step = rest[..index].to_string();
        let list = &after[digits_end + 1 + "failed:".len()..];
        let tests = list
            .split(", ")
            .map(str::trim)
            .filter(|test| !test.is_empty())
            .map(str::to_string)
            .collect();
        return Some((step, tests));
    }
    None
}

/// Parse a Forgejo status description into a failure summary.
///
/// Accepted shapes (needle-ci's publish-failure-summary is the author):
/// - `needle-ci Failed: <step>: <N> failed: <test, test, ...>`
/// - `needle-ci Failed: <step>` (no test list — build steps, fallback posts)
/// - any other text: treated as a bare step so an unrecognised shape still
///   fingerprints distinctly instead of collapsing into one bucket.
pub fn parse_failure_description(description: &str) -> FailureSummary {
    let rest = description
        .trim()
        .strip_prefix("needle-ci")
        .map(str::trim_start);
    // Drop the status word (`Failed`, `Succeeded`, …) when present.
    let rest = match rest {
        Some(rest) => match rest.split_once(':') {
            Some((_status, tail)) => tail.trim_start(),
            None => rest,
        },
        None => description.trim(),
    };
    if rest.is_empty() {
        return FailureSummary::default();
    }
    match split_step_and_tests(rest) {
        Some((step, tests)) => FailureSummary {
            failing_step: step,
            failing_test_ids: tests,
        },
        None => FailureSummary {
            failing_step: rest.to_string(),
            failing_test_ids: Vec::new(),
        },
    }
}

/// Derive the summary from a run when the Forgejo description is absent —
/// the Argo fallback. The failing step is the first failed Pod node's
/// display name; test ids are not recoverable from the workflow API.
fn summary_from_failed_nodes(run: &CiWorkflowRun) -> FailureSummary {
    FailureSummary {
        failing_step: run
            .failed_nodes
            .first()
            .map(|node| node.display_name.clone())
            .unwrap_or_else(|| run.message.clone().unwrap_or_default()),
        failing_test_ids: Vec::new(),
    }
}

/// Fingerprint a red verdict: the step plus the sorted test ids.
///
/// Sorted so a shard reordering the same failures fingerprints identically;
/// the separator is a control character that cannot appear in a step or test
/// id, keeping the two fields unambiguously delimited.
pub fn failure_fingerprint(step: &str, test_ids: &[String]) -> String {
    let mut sorted = test_ids.to_vec();
    sorted.sort();
    let mut hasher = Sha256::new();
    hasher.update(step.as_bytes());
    hasher.update([0x1f]);
    for test in &sorted {
        hasher.update(test.as_bytes());
        hasher.update([0x1f]);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// ─── Verdicts ───────────────────────────────────────────────────────────────

/// One completed CI verdict on a revision, reduced to what filing needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiVerdict {
    /// The revision the verdict is about.
    pub revision: String,
    /// Whether the verdict is red (failing). A green verdict never files.
    pub red: bool,
    /// Identity of the status record behind the verdict: two verdicts that
    /// differ only by `status_key` are two observations (for example a
    /// re-run), while an unchanged key is the same observation again.
    pub status_key: String,
    /// Workflow name as published (for example `iad-ci/needle-ci`).
    pub workflow: String,
    /// Parsed failure summary.
    pub summary: FailureSummary,
    /// The raw status text, kept for notes and bead bodies.
    pub description: String,
    /// Source that supplied the verdict (`forgejo` or `argo`).
    pub source: String,
}

impl CiVerdict {
    /// Reduce a [`CiWorkflowRun`] to a verdict, or `None` when the run has
    /// no completed verdict (pending, running, unknown phase).
    fn from_run(revision: &str, run: &CiWorkflowRun) -> Option<Self> {
        let red = match run.status() {
            BuildStatus::Passing => false,
            BuildStatus::Failing => true,
            BuildStatus::Unknown => return None,
        };
        let is_forgejo = run.source.as_deref() == Some("forgejo");
        let summary = if is_forgejo {
            parse_failure_description(run.message.as_deref().unwrap_or(""))
        } else {
            summary_from_failed_nodes(run)
        };
        let description = if is_forgejo {
            run.message.clone().unwrap_or_default()
        } else if !run.failed_node_signature().is_empty() {
            run.failed_node_signature()
        } else {
            run.message.clone().unwrap_or_default()
        };
        Some(Self {
            revision: revision.to_string(),
            red,
            status_key: format!(
                "{}|{}|{}|{}",
                run.source.as_deref().unwrap_or("?"),
                run.name,
                run.created_at
                    .map(|stamp| stamp.to_rfc3339())
                    .unwrap_or_default(),
                run.phase,
            ),
            workflow: run.name.clone(),
            summary,
            description,
            source: run.source.clone().unwrap_or_else(|| "?".to_string()),
        })
    }

    /// The unique-ref the backend deduplicates on for this fingerprint.
    fn unique_ref(&self, fingerprint: &str) -> String {
        format!("{UNIQUE_REF_NAMESPACE}:{fingerprint}")
    }

    /// The `<test or step>` half of the bead title: the first sorted failing
    /// test when the summary carried test ids, else the failing step.
    fn title_subject(&self) -> String {
        let mut tests = self.summary.failing_test_ids.clone();
        tests.sort();
        tests
            .into_iter()
            .next()
            .unwrap_or_else(|| self.summary.failing_step.clone())
    }

    /// Bead title per the filing contract.
    fn bead_title(&self) -> String {
        format!("needle-ci red on main: {}", self.title_subject())
    }
}

/// Supplies the newest completed CI verdict for a revision.
///
/// The trait exists so tests can script verdicts without a live Forgejo or
/// cluster; production uses [`ForgejoFirstVerdictSource`].
#[async_trait::async_trait]
pub trait CiVerdictSource: Send + Sync {
    /// Newest completed verdict for `revision`, or `None` when nothing
    /// verdict-shaped is visible for it.
    async fn verdict_for(
        &self,
        workspace: &Path,
        template: &str,
        revision: &str,
    ) -> Result<Option<CiVerdict>>;
}

/// Resolve recent `origin/main` revisions, newest first.
///
/// `ls-remote` supplies the authoritative remote tip. The shared checkout
/// normally already has every pushed object, so `rev-list` can walk that tip
/// without mutating refs. If the remote advertises an object this checkout
/// does not have yet, fall back to the tip itself and retry history on the
/// next worker cycle after the checkout catches up.
async fn recent_main_revisions(workspace: &Path) -> Option<Vec<String>> {
    let tip = main_branch_sha(workspace).await?;
    let max_count = format!("--max-count={VERDICT_HISTORY_LIMIT}");
    let output = tokio::process::Command::new("git")
        .args(["rev-list", &max_count, &tip])
        .current_dir(workspace)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return Some(vec![tip]);
    }
    let mut revisions: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|revision| !revision.is_empty())
        .map(str::to_string)
        .collect();
    if revisions.first() != Some(&tip) {
        revisions.insert(0, tip);
    }
    Some(revisions)
}

/// Source for the Forgejo commit status needle-ci publishes.
pub struct ForgejoFirstVerdictSource {
    forgejo: ForgejoStatusSource,
}

impl ForgejoFirstVerdictSource {
    /// Source pointed at the configured Forgejo endpoint.
    pub fn production() -> Self {
        Self {
            forgejo: ForgejoStatusSource::from_env(),
        }
    }

    /// Source with explicit endpoints (tests embed a fake Forgejo here).
    pub fn new(forgejo_base_url: String) -> Self {
        Self {
            forgejo: ForgejoStatusSource::with_base_url_no_auth(forgejo_base_url),
        }
    }
}

#[async_trait::async_trait]
impl CiVerdictSource for ForgejoFirstVerdictSource {
    async fn verdict_for(
        &self,
        workspace: &Path,
        template: &str,
        revision: &str,
    ) -> Result<Option<CiVerdict>> {
        let run = self
            .forgejo
            .newest_run_for_workspace(workspace, template, Some(revision))
            .await?;
        Ok(run
            .as_ref()
            .and_then(|run| CiVerdict::from_run(revision, run)))
    }
}

// ─── Persistent state ───────────────────────────────────────────────────────

/// The verdict identity last processed, for idempotent re-polls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservedVerdict {
    pub revision: String,
    pub red: bool,
    pub fingerprint: Option<String>,
    pub status_key: String,
}

/// A fingerprint whose red has a bead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FiledRed {
    pub bead_id: String,
    pub first_revision: String,
    /// Status key of the newest red already noted on the bead.
    pub last_noted: Option<String>,
}

/// Persisted state for the ci-watch strand.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CiWatchState {
    pub last_poll: Option<DateTime<Utc>>,
    pub last_verdict: Option<ObservedVerdict>,
    /// Fingerprint → filed bead.
    pub filed: HashMap<String, FiledRed>,
}

impl CiWatchState {
    /// Load state from disk, returning default if the file doesn't exist.
    fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(data) => serde_json::from_str(&data)
                .with_context(|| format!("failed to parse ci-watch state: {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => {
                Err(e).with_context(|| format!("failed to read ci-watch state: {}", path.display()))
            }
        }
    }

    /// Persist state to disk.
    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create ci-watch state dir: {}", parent.display())
            })?;
        }
        let data =
            serde_json::to_string_pretty(self).context("failed to serialize ci-watch state")?;
        std::fs::write(path, data)
            .with_context(|| format!("failed to write ci-watch state: {}", path.display()))
    }
}

// ─── The strand ─────────────────────────────────────────────────────────────

/// The ci-watch strand.
pub struct CiWatchStrand {
    config: CiWatchConfig,
    workspace: PathBuf,
    state_dir: PathBuf,
    telemetry: Telemetry,
    source: std::sync::Arc<dyn CiVerdictSource>,
}

impl CiWatchStrand {
    /// Create the strand with the production verdict source.
    pub fn new(
        config: CiWatchConfig,
        workspace: PathBuf,
        state_dir: PathBuf,
        telemetry: Telemetry,
    ) -> Self {
        Self {
            config,
            workspace,
            state_dir,
            telemetry,
            source: std::sync::Arc::new(ForgejoFirstVerdictSource::production()),
        }
    }

    /// Create the strand with an injected verdict source (tests).
    pub fn with_source(
        config: CiWatchConfig,
        workspace: PathBuf,
        state_dir: PathBuf,
        telemetry: Telemetry,
        source: std::sync::Arc<dyn CiVerdictSource>,
    ) -> Self {
        Self {
            config,
            workspace,
            state_dir,
            telemetry,
            source,
        }
    }

    fn state_file_path(&self) -> PathBuf {
        self.state_dir
            .join(format!("{}.json", workspace_hash(&self.workspace)))
    }

    fn emit(&self, level: &str, context: serde_json::Value) {
        self.telemetry
            .emit(
                EventKind::Log {
                    phase: PHASE.to_string(),
                    level: level.to_string(),
                    bead_id: None,
                    context,
                },
                Utc::now(),
            )
            .map_err(|error| {
                tracing::warn!(error = %error, "ci-watch telemetry emit failed");
                error
            })
            .ok();
    }

    /// Best-effort parent status for the bead body: green / red / none /
    /// unknown. The parent SHA comes from the local checkout, which may lag
    /// `origin/main` — an absent object reports `unknown` rather than
    /// blocking the filing.
    async fn parent_status(&self, template: &str, revision: &str) -> String {
        let output = tokio::process::Command::new("git")
            .args(["rev-parse", "--verify", &format!("{revision}^")])
            .current_dir(&self.workspace)
            .output()
            .await;
        let parent = match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => return "unknown".to_string(),
        };
        match self
            .source
            .verdict_for(&self.workspace, template, &parent)
            .await
        {
            Ok(Some(verdict)) if verdict.red => "red".to_string(),
            Ok(Some(_)) => "green".to_string(),
            Ok(None) => "none".to_string(),
            Err(_) => "unknown".to_string(),
        }
    }

    /// Compose the body of a first-filing bead.
    fn red_body(verdict: &CiVerdict, parent_status: &str, fingerprint: &str) -> String {
        let tests = if verdict.summary.failing_test_ids.is_empty() {
            "(none reported)".to_string()
        } else {
            let mut sorted = verdict.summary.failing_test_ids.clone();
            sorted.sort();
            sorted.join(", ")
        };
        format!(
            "Auto-filed by needle's ci-watch strand: needle-ci is red on main.\n\
             \n\
             - First failing revision: {}\n\
             - Parent revision status: {}\n\
             - Workflow: {}\n\
             - Failing step: {}\n\
             - Failing tests: {}\n\
             - CI status text: {}\n\
             - Failure fingerprint: {}\n\
             - Dedupe ref: {}\n\
             \n\
             A later red with the same fingerprint appends a note here instead of \
             filing a new bead. This bead is never auto-closed: ci-watch files and \
             notes only — close it once main is green with the fix landed.",
            verdict.revision,
            parent_status,
            verdict.workflow,
            verdict.summary.failing_step,
            tests,
            verdict.description,
            fingerprint,
            verdict.unique_ref(fingerprint),
        )
    }

    /// File the first bead for a fingerprint. An `EXISTING` result means
    /// another worker won the create race: fall through to the recurrence
    /// note so the winner's bead records this observation too.
    async fn file_new_red(
        &self,
        store: &dyn BeadStore,
        verdict: &CiVerdict,
        fingerprint: &str,
        template: &str,
        state: &mut CiWatchState,
    ) -> Result<StrandResult> {
        let parent_status = self.parent_status(template, &verdict.revision).await;
        let body = Self::red_body(verdict, &parent_status, fingerprint);
        let unique_ref = verdict.unique_ref(fingerprint);
        let created = store
            .create_bead_with_unique_ref(
                &verdict.bead_title(),
                &body,
                &RED_LABELS,
                RED_PRIORITY,
                &unique_ref,
            )
            .await?;
        match created {
            DedupCreate::Created(id) => {
                state.filed.insert(
                    fingerprint.to_string(),
                    FiledRed {
                        bead_id: id.to_string(),
                        first_revision: verdict.revision.clone(),
                        last_noted: None,
                    },
                );
                self.emit(
                    "info",
                    serde_json::json!({
                        "action": "filed",
                        "bead_id": id.to_string(),
                        "revision": verdict.revision,
                        "fingerprint": fingerprint,
                        "parent_status": parent_status,
                        "workflow": verdict.workflow,
                        "source": verdict.source,
                    }),
                );
                tracing::info!(
                    bead = %id,
                    revision = %verdict.revision,
                    fingerprint,
                    "ci-watch filed red bead"
                );
                Ok(StrandResult::WorkCreated)
            }
            DedupCreate::Existed(id) => {
                tracing::debug!(
                    bead = %id,
                    fingerprint,
                    "ci-watch create race lost; treating as recurrence"
                );
                self.note_recurring_red(store, verdict, fingerprint, &id, state)
                    .await
            }
            DedupCreate::ExistedClosed(id) => {
                // The fingerprint's bead was closed once: this red is a
                // regression of fixed work. Note it and reopen the bead so
                // it is claimable again (reopen clears the assignee). There
                // is no prior filing in this worker's state — the backend's
                // unique-ref is the durable dedupe — so this revision is
                // both the first seen here and the one recorded.
                let note = recurrence_note(verdict, &verdict.revision.clone());
                store.append_notes(&id, &note).await?;
                store.reopen(&id).await?;
                state.filed.insert(
                    fingerprint.to_string(),
                    FiledRed {
                        bead_id: id.to_string(),
                        first_revision: verdict.revision.clone(),
                        last_noted: Some(verdict.status_key.clone()),
                    },
                );
                self.emit(
                    "warn",
                    serde_json::json!({
                        "action": "reopened",
                        "bead_id": id.to_string(),
                        "revision": verdict.revision,
                        "fingerprint": fingerprint,
                    }),
                );
                Ok(StrandResult::WorkCreated)
            }
        }
    }

    /// Append the recurrence note for a fingerprint that already has a bead.
    /// When that bead has since been closed, reopen it so the regression is
    /// claimable — filing a second bead for one fingerprint is exactly what
    /// the dedupe contract forbids.
    async fn note_recurring_red(
        &self,
        store: &dyn BeadStore,
        verdict: &CiVerdict,
        fingerprint: &str,
        bead_id: &BeadId,
        state: &mut CiWatchState,
    ) -> Result<StrandResult> {
        let first_revision = state
            .filed
            .get(fingerprint)
            .map(|filed| filed.first_revision.clone())
            .unwrap_or_else(|| verdict.revision.clone());
        if state
            .filed
            .get(fingerprint)
            .and_then(|filed| filed.last_noted.as_ref())
            == Some(&verdict.status_key)
        {
            // This exact status record was already noted; nothing to add.
            return Ok(StrandResult::NoWork);
        }
        let closed = match store.show(bead_id).await {
            Ok(bead) => matches!(bead.status, BeadStatus::Done | BeadStatus::Closed),
            Err(error) => {
                // An unreadable bead still gets its note; skipping it would
                // silently drop the observation.
                tracing::warn!(error = %error, bead = %bead_id, "ci-watch could not read filed bead");
                false
            }
        };
        let note = recurrence_note(verdict, &first_revision);
        store.append_notes(bead_id, &note).await?;
        if closed {
            store.reopen(bead_id).await?;
        }
        if let Some(filed) = state.filed.get_mut(fingerprint) {
            filed.last_noted = Some(verdict.status_key.clone());
        } else {
            state.filed.insert(
                fingerprint.to_string(),
                FiledRed {
                    bead_id: bead_id.to_string(),
                    first_revision,
                    last_noted: Some(verdict.status_key.clone()),
                },
            );
        }
        self.emit(
            "info",
            serde_json::json!({
                "action": if closed { "noted_reopened" } else { "noted" },
                "bead_id": bead_id.to_string(),
                "revision": verdict.revision,
                "fingerprint": fingerprint,
            }),
        );
        Ok(if closed {
            StrandResult::WorkCreated
        } else {
            StrandResult::NoWork
        })
    }
}

/// The note appended when a fingerprint that already has a bead goes red
/// again on a new revision.
fn recurrence_note(verdict: &CiVerdict, first_revision: &str) -> String {
    format!(
        "ci-watch: still red at {} ({}): {} — first filed at {}",
        verdict.revision, verdict.workflow, verdict.description, first_revision
    )
}

/// Stable per-workspace state filename component (same scheme as pulse).
fn workspace_hash(workspace: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(workspace.display().to_string().as_bytes());
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[async_trait::async_trait]
impl super::Strand for CiWatchStrand {
    fn name(&self) -> &str {
        "ci_watch"
    }

    fn is_generator(&self) -> bool {
        true
    }

    async fn evaluate(&self, store: &dyn BeadStore, _exclusions: &HashSet<BeadId>) -> StrandResult {
        if !self.config.enabled {
            return StrandResult::NoWork;
        }

        let state_path = self.state_file_path();
        let mut state = match CiWatchState::load(&state_path) {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(error = %error, "ci-watch state load failed, using defaults");
                CiWatchState::default()
            }
        };

        // Poll gate: within the interval there is nothing to do, and the
        // gate must not touch the network.
        if let Some(last_poll) = state.last_poll {
            let elapsed = Utc::now().signed_duration_since(last_poll).num_seconds();
            if elapsed < self.config.poll_interval_secs as i64 {
                return StrandResult::NoWork;
            }
        }

        // Advance the poll stamp first and persist only the backoff on any
        // failure below, so a persistently failing endpoint is not hammered
        // every cycle while the red itself stays unprocessed and retried.
        let backoff = |state: &CiWatchState| {
            let mut backoff_state = state.clone();
            backoff_state.last_poll = Some(Utc::now());
            if let Err(error) = backoff_state.save(&state_path) {
                tracing::warn!(error = %error, "ci-watch backoff save failed");
            }
        };

        let template = match template_for_workspace(&self.workspace).await {
            Ok(template) => template,
            Err(error) => {
                tracing::warn!(error = %error, "ci-watch could not name the CI template");
                backoff(&state);
                return StrandResult::NoWork;
            }
        };
        let Some(revisions) = recent_main_revisions(&self.workspace).await else {
            tracing::warn!("ci-watch could not resolve origin/main");
            backoff(&state);
            return StrandResult::NoWork;
        };
        let mut verdict = None;
        for revision in &revisions {
            match self
                .source
                .verdict_for(&self.workspace, &template, revision)
                .await
            {
                Ok(Some(found)) => {
                    verdict = Some(found);
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(error = %error, revision, "ci-watch verdict source failed");
                    self.emit(
                        "warn",
                        serde_json::json!({ "action": "source_error", "revision": revision }),
                    );
                    backoff(&state);
                    return StrandResult::NoWork;
                }
            }
        }
        let Some(verdict) = verdict else {
            // No completed verdict in the recent mainline window yet.
            backoff(&state);
            return StrandResult::NoWork;
        };

        let fingerprint = verdict.red.then(|| {
            failure_fingerprint(
                &verdict.summary.failing_step,
                &verdict.summary.failing_test_ids,
            )
        });
        let observed = ObservedVerdict {
            revision: verdict.revision.clone(),
            red: verdict.red,
            fingerprint: fingerprint.clone(),
            status_key: verdict.status_key.clone(),
        };
        if state.last_verdict.as_ref() == Some(&observed) {
            // Identical re-observation (same revision, same status record):
            // a no-op even before consulting the store.
            backoff(&state);
            return StrandResult::NoWork;
        }

        let outcome = if !verdict.red {
            // Green: record only. Beads are never closed by this strand.
            Ok(StrandResult::NoWork)
        } else {
            let fingerprint = fingerprint.clone().unwrap_or_default();
            match state.filed.get(&fingerprint) {
                None => {
                    self.file_new_red(store, &verdict, &fingerprint, &template, &mut state)
                        .await
                }
                Some(filed) => {
                    let bead_id = BeadId::from(filed.bead_id.clone());
                    self.note_recurring_red(store, &verdict, &fingerprint, &bead_id, &mut state)
                        .await
                }
            }
        };

        match outcome {
            Ok(result) => {
                state.last_verdict = Some(observed);
                state.last_poll = Some(Utc::now());
                if let Err(error) = state.save(&state_path) {
                    tracing::warn!(error = %error, "ci-watch state save failed");
                }
                result
            }
            Err(error) => {
                // The red was seen but not recorded against a bead; leave
                // `last_verdict` unset so the next interval retries it.
                tracing::error!(error = %error, "ci-watch failed to record red verdict");
                self.emit(
                    "error",
                    serde_json::json!({
                        "action": "record_error",
                        "revision": verdict.revision,
                        "fingerprint": fingerprint,
                    }),
                );
                backoff(&state);
                StrandResult::NoWork
            }
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_status::FailedNode;
    use crate::strand::Strand;
    use crate::types::Bead;
    use anyhow::bail;

    fn run(name: &str, phase: &str, message: Option<&str>, source: Option<&str>) -> CiWorkflowRun {
        CiWorkflowRun {
            name: name.to_string(),
            phase: phase.to_string(),
            created_at: Some(Utc::now()),
            message: message.map(str::to_string),
            failed_nodes: Vec::new(),
            commit_sha: Some("abc123".to_string()),
            source: source.map(str::to_string),
        }
    }

    #[test]
    fn parse_full_failure_description() {
        let summary = parse_failure_description(
            "needle-ci Failed: verify: 2 failed: tests::alpha, tests::beta",
        );
        assert_eq!(summary.failing_step, "verify");
        assert_eq!(
            summary.failing_test_ids,
            vec!["tests::alpha".to_string(), "tests::beta".to_string()]
        );
    }

    #[test]
    fn parse_step_only_description() {
        let summary = parse_failure_description(
            "needle-ci Failed: publish-failure-summary (summary unavailable)",
        );
        assert_eq!(
            summary.failing_step,
            "publish-failure-summary (summary unavailable)"
        );
        assert!(summary.failing_test_ids.is_empty());
    }

    #[test]
    fn parse_truncated_test_list_keeps_partial_id() {
        let summary = parse_failure_description(
            "needle-ci Failed: verify: 2 failed: tests::alpha, tests::be",
        );
        // The 140-char API truncation can cut the last test id mid-name; the
        // partial id stays so the fingerprint is stable against the same cut.
        assert_eq!(
            summary.failing_test_ids,
            vec!["tests::alpha".to_string(), "tests::be".to_string()]
        );
    }

    #[test]
    fn parse_unrecognised_description_fingerprints_as_bare_step() {
        let summary = parse_failure_description("some other CI said no");
        assert_eq!(summary.failing_step, "some other CI said no");
        assert!(summary.failing_test_ids.is_empty());
    }

    #[test]
    fn fingerprint_is_test_order_insensitive_and_step_sensitive() {
        let a = failure_fingerprint("verify", &["b".into(), "a".into()]);
        let b = failure_fingerprint("verify", &["a".into(), "b".into()]);
        let c = failure_fingerprint("build", &["a".into(), "b".into()]);
        assert_eq!(
            a, b,
            "same failures in a different order are one fingerprint"
        );
        assert_ne!(a, c, "a different step is a different fingerprint");
    }

    #[test]
    fn verdict_from_forgejo_run_parses_description() {
        let run = run(
            "iad-ci/needle-ci",
            "Failed",
            Some("needle-ci Failed: verify: 1 failed: tests::alpha"),
            Some("forgejo"),
        );
        let verdict = CiVerdict::from_run("deadbeef", &run).expect("failed run is a verdict");
        assert!(verdict.red);
        assert_eq!(verdict.summary.failing_step, "verify");
        assert_eq!(verdict.summary.failing_test_ids, vec!["tests::alpha"]);
        assert_eq!(verdict.bead_title(), "needle-ci red on main: tests::alpha");
        assert_eq!(
            verdict.unique_ref("abc123def4567890"),
            "needle-ci-red:abc123def4567890"
        );
    }

    #[test]
    fn verdict_from_argo_run_names_step_from_failed_node() {
        let mut argo_run = run("needle-ci-xyz", "Failed", Some("terminated"), Some("argo"));
        argo_run.failed_nodes = vec![FailedNode {
            display_name: "verify".to_string(),
            message: "exit code 1".to_string(),
        }];
        let verdict = CiVerdict::from_run("deadbeef", &argo_run).expect("failed run is a verdict");
        assert!(verdict.red);
        assert_eq!(verdict.summary.failing_step, "verify");
        assert_eq!(verdict.summary.failing_test_ids, Vec::<String>::new());
    }

    #[test]
    fn pending_run_is_not_a_verdict() {
        let run = run("iad-ci/needle-ci", "Running", None, Some("forgejo"));
        assert!(CiVerdict::from_run("deadbeef", &run).is_none());
    }

    #[test]
    fn green_run_is_a_non_red_verdict() {
        let run = run(
            "iad-ci/needle-ci",
            "Succeeded",
            Some("needle-ci Succeeded"),
            Some("forgejo"),
        );
        let verdict = CiVerdict::from_run("deadbeef", &run).expect("succeeded run is a verdict");
        assert!(!verdict.red);
        assert!(!verdict.bead_title().is_empty());
    }

    #[test]
    fn rerun_status_key_differs() {
        let first = run("iad-ci/needle-ci", "Failed", Some("a"), Some("forgejo"));
        let mut second = first.clone();
        second.created_at = Some(Utc::now() + chrono::Duration::seconds(60));
        let key_a = CiVerdict::from_run("r", &first).unwrap().status_key;
        let key_b = CiVerdict::from_run("r", &second).unwrap().status_key;
        assert_ne!(key_a, key_b, "a re-run is a new observation");
    }

    struct ScriptedSource {
        verdicts: std::sync::Mutex<Vec<Option<CiVerdict>>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl CiVerdictSource for ScriptedSource {
        async fn verdict_for(
            &self,
            _workspace: &Path,
            _template: &str,
            _revision: &str,
        ) -> Result<Option<CiVerdict>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut verdicts = self.verdicts.lock().unwrap();
            Ok(verdicts.pop().unwrap_or(None))
        }
    }

    struct NilStore;

    #[async_trait::async_trait]
    impl crate::bead_store::BeadStore for NilStore {
        async fn ready(&self, _filters: &crate::bead_store::Filters) -> Result<Vec<Bead>> {
            Ok(Vec::new())
        }
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(Vec::new())
        }
        async fn starvation_inventory(&self) -> Result<Vec<Bead>> {
            Ok(Vec::new())
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            bail!("nil store")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<crate::types::ClaimResult> {
            bail!("nil store")
        }
        async fn claim_auto(&self, _actor: &str) -> Result<crate::types::ClaimResult> {
            bail!("nil store")
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn block(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
        async fn reopen(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            bail!("nil store")
        }
        async fn add_dependency(&self, _blocker: &BeadId, _blocked: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(&self, _blocked: &BeadId, _blocker: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn doctor_repair(&self) -> Result<crate::bead_store::RepairReport> {
            Ok(Default::default())
        }
        async fn doctor_check(&self) -> Result<crate::bead_store::RepairReport> {
            Ok(Default::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
        fn has_valid_store(&self) -> bool {
            false
        }
    }

    fn strand_with(
        source: std::sync::Arc<dyn CiVerdictSource>,
        state_dir: &Path,
        interval: u64,
    ) -> CiWatchStrand {
        CiWatchStrand::with_source(
            CiWatchConfig {
                enabled: true,
                poll_interval_secs: interval,
            },
            PathBuf::from("/nonexistent-workspace"),
            state_dir.to_path_buf(),
            Telemetry::new("ci-watch-test".to_string()),
            source,
        )
    }

    #[tokio::test]
    async fn poll_gate_skips_without_touching_the_source() {
        let state_dir = tempfile::tempdir().unwrap();
        let source = std::sync::Arc::new(ScriptedSource {
            verdicts: std::sync::Mutex::new(vec![]),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        // Seed a fresh poll so the gate holds.
        std::fs::create_dir_all(state_dir.path()).unwrap();
        let seeded = CiWatchState {
            last_poll: Some(Utc::now()),
            ..Default::default()
        };
        seeded
            .save(&state_dir.path().join(format!(
                "{}.json",
                workspace_hash(Path::new("/nonexistent-workspace"))
            )))
            .unwrap();

        let strand = strand_with(source.clone(), state_dir.path(), 300);
        let result = strand.evaluate(&NilStore, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
        assert_eq!(
            source.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the poll gate must not touch the verdict source"
        );
    }
}
