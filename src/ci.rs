//! Asynchronous post-push CI lifecycle.
//!
//! A worker only registers a pushed commit and returns its slot.  A separate
//! reconciler polls the configured Forgejo/Argo authority and advances the
//! durable state machine.  The ledger is append-only so a process restart can
//! recover a check after the bead was created but before the transition was
//! recorded, and so Argo pod/log retention never destroys the evidence.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::bead_store::BeadStore;
use crate::config::PostPushCiConfig;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{Bead, BeadId};

const CHECK_MARKER: &str = "needle-ci-check:v1";
const REPAIR_MARKER: &str = "needle-ci-repair:v1";
const CHECK_LABEL: &str = "ci-check";
const REPAIR_LABEL: &str = "ci-repair";
const LEDGER_FILE: &str = "lifecycle.jsonl";
const MAX_SUMMARY_BYTES: usize = 2048;

/// Identity of one authoritative workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CiCheckKey {
    pub repository: String,
    pub commit_sha: String,
    pub workflow: String,
}

impl CiCheckKey {
    fn id(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}",
            self.repository, self.commit_sha, self.workflow
        )
    }
}

/// Correlation failure is deliberately explicit: guessing a parent can attach
/// CI results to another agent's work in a shared checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrelationError {
    MissingTrailer {
        commit_sha: String,
    },
    AmbiguousTrailers {
        commit_sha: String,
        parents: Vec<BeadId>,
    },
    TrailerMismatch {
        expected: BeadId,
        found: BeadId,
    },
}

impl std::fmt::Display for CorrelationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingTrailer { commit_sha } => {
                write!(f, "commit {commit_sha} has no Bead-Id trailer")
            }
            Self::AmbiguousTrailers {
                commit_sha,
                parents,
            } => {
                write!(
                    f,
                    "commit {commit_sha} has ambiguous Bead-Id trailers: {parents:?}"
                )
            }
            Self::TrailerMismatch { expected, found } => {
                write!(f, "commit trailer identifies {found}, expected {expected}")
            }
        }
    }
}

impl std::error::Error for CorrelationError {}

/// Result returned to a worker after it inspects the post-dispatch checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationResult {
    Disabled,
    NoPushedCommit,
    Registered { key: CiCheckKey, check_id: BeadId },
    CorrelationFailed(CorrelationError),
}

/// Classification used by the reconciler.  Only `ProductFailure` creates a
/// code repair bead.  Infrastructure, timeout, and unavailable-result cases
/// remain on a retry path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiFailureClass {
    Product,
    Infrastructure,
    Timeout,
}

/// A normalized result from Forgejo/Argo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiObservation {
    Pending {
        run_reference: Option<String>,
        event_id: Option<String>,
        summary: String,
    },
    Success {
        run_reference: Option<String>,
        log_reference: Option<String>,
        event_id: Option<String>,
        summary: String,
    },
    Failure {
        class: CiFailureClass,
        run_reference: Option<String>,
        log_reference: Option<String>,
        event_id: Option<String>,
        summary: String,
    },
}

/// Source abstraction makes webhook delivery and bounded polling share the
/// same state machine.  The production implementation below reads the
/// configured Forgejo/Argo endpoint; tests and operators can inject another
/// authoritative source without changing bead transitions.
#[async_trait]
pub trait CiResultSource: Send + Sync {
    async fn poll(&self, key: &CiCheckKey) -> Result<CiObservation>;
}

/// Generic Forgejo/Argo JSON source.
pub struct ForgejoArgoResultSource {
    config: PostPushCiConfig,
}

impl ForgejoArgoResultSource {
    pub fn new(config: PostPushCiConfig) -> Result<Self> {
        if !config.enabled {
            bail!("post_push_ci is disabled")
        }
        if config.result_url_template.is_none()
            && config
                .repositories
                .values()
                .all(|repo| repo.result_url_template.is_none())
        {
            bail!("post_push_ci has no Forgejo/Argo result_url_template")
        }
        Ok(Self { config })
    }

    fn endpoint(&self, key: &CiCheckKey) -> Option<String> {
        let repo = self.config.repositories.get(&key.repository);
        let template = repo
            .and_then(|entry| entry.result_url_template.as_ref())
            .or(self.config.result_url_template.as_ref())?;
        Some(render_template(template, key))
    }
}

#[async_trait]
impl CiResultSource for ForgejoArgoResultSource {
    async fn poll(&self, key: &CiCheckKey) -> Result<CiObservation> {
        let Some(endpoint) = self.endpoint(key) else {
            return Ok(CiObservation::Failure {
                class: CiFailureClass::Infrastructure,
                run_reference: None,
                log_reference: None,
                event_id: None,
                summary: "no authoritative CI result endpoint is configured".to_string(),
            });
        };
        let auth_env = self.config.auth_token_env.clone();
        let request_url = endpoint.clone();
        let response = match tokio::task::spawn_blocking(move || {
            let mut request = ureq::get(&request_url).timeout(std::time::Duration::from_secs(30));
            // Read credentials only inside the request task.  They never enter
            // a ledger entry, telemetry payload, error, or command argument.
            if let Some(env_name) = auth_env {
                if let Ok(token) = std::env::var(env_name) {
                    request = request.set("Authorization", &format!("Bearer {token}"));
                }
            }
            match request.call() {
                Ok(response) => Ok(response.into_string()?),
                Err(ureq::Error::Status(404, _)) => Ok(String::new()),
                Err(error) => Err(anyhow::anyhow!(error.to_string())),
            }
        })
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(_)) | Err(_) => {
                return Ok(CiObservation::Failure {
                    class: CiFailureClass::Infrastructure,
                    run_reference: Some(strip_query(&endpoint)),
                    log_reference: None,
                    event_id: None,
                    summary: "authoritative CI result request failed; retry scheduled".to_string(),
                });
            }
        };

        if response.trim().is_empty() {
            return Ok(CiObservation::Pending {
                run_reference: Some(strip_query(&endpoint)),
                event_id: None,
                summary: "authoritative CI run has not appeared yet".to_string(),
            });
        }
        let value: serde_json::Value = match serde_json::from_str(&response) {
            Ok(value) => value,
            Err(_) => {
                return Ok(CiObservation::Failure {
                    class: CiFailureClass::Infrastructure,
                    run_reference: Some(strip_query(&endpoint)),
                    log_reference: None,
                    event_id: None,
                    summary: "authoritative CI result was not valid JSON; retry scheduled"
                        .to_string(),
                });
            }
        };
        Ok(parse_authoritative_result(&value, Some(endpoint)))
    }
}

/// Parse the common Forgejo check-run and Argo Workflow status shapes.
pub fn parse_authoritative_result(
    value: &serde_json::Value,
    default_reference: Option<String>,
) -> CiObservation {
    let status = first_string(
        value,
        &[
            &["status", "phase"],
            &["status", "conclusion"],
            &["conclusion"],
            &["state"],
            &["status"],
        ],
    )
    .unwrap_or("pending")
    .to_ascii_lowercase();
    let reference = first_string(
        value,
        &[
            &["run_url"],
            &["html_url"],
            &["url"],
            &["metadata", "selfLink"],
        ],
    )
    .map(strip_query)
    .or(default_reference.map(|url| strip_query(&url)));
    let log_reference = first_string(value, &[&["log_url"], &["logs_url"]]).map(strip_query);
    let event_id =
        first_string(value, &[&["id"], &["run_id"], &["metadata", "name"]]).map(ToOwned::to_owned);
    let summary = first_string(value, &[&["summary"], &["message"], &["status", "message"]])
        .unwrap_or(status.as_str())
        .to_string();
    let summary = sanitize_summary(&summary);

    if matches!(
        status.as_str(),
        "success" | "succeeded" | "passed" | "completed"
    ) {
        return CiObservation::Success {
            run_reference: reference,
            log_reference,
            event_id,
            summary,
        };
    }

    if matches!(
        status.as_str(),
        "failure" | "failed" | "failure_product" | "cancelled"
    ) {
        let class = first_string(
            value,
            &[
                &["failure_class"],
                &["classification"],
                &["reason"],
                &["status", "message"],
            ],
        )
        .map(|value| value.to_ascii_lowercase())
        .map(|value| {
            if value.contains("timeout")
                || value.contains("timed out")
                || value.contains("deadline")
            {
                CiFailureClass::Timeout
            } else if value.contains("infra")
                || value.contains("transient")
                || value.contains("network")
                || value.contains("unavailable")
                || value.contains("evict")
                || value.contains("capacity")
                || value.contains("runner")
            {
                CiFailureClass::Infrastructure
            } else {
                CiFailureClass::Product
            }
        })
        .unwrap_or(if status == "cancelled" {
            CiFailureClass::Infrastructure
        } else {
            CiFailureClass::Product
        });
        return CiObservation::Failure {
            class,
            run_reference: reference,
            log_reference,
            event_id,
            summary,
        };
    }

    if matches!(
        status.as_str(),
        "timeout" | "timed_out" | "deadline_exceeded"
    ) {
        return CiObservation::Failure {
            class: CiFailureClass::Timeout,
            run_reference: reference,
            log_reference,
            event_id,
            summary,
        };
    }

    if matches!(
        status.as_str(),
        "error" | "errored" | "infrastructure_failure" | "unavailable"
    ) {
        return CiObservation::Failure {
            class: CiFailureClass::Infrastructure,
            run_reference: reference,
            log_reference,
            event_id,
            summary,
        };
    }

    CiObservation::Pending {
        run_reference: reference,
        event_id,
        summary,
    }
}

fn first_string<'a>(value: &'a serde_json::Value, paths: &[&[&str]]) -> Option<&'a str> {
    paths.iter().find_map(|path| {
        let mut current = value;
        for segment in *path {
            current = current.get(*segment)?;
        }
        current.as_str()
    })
}

fn render_template(template: &str, key: &CiCheckKey) -> String {
    template
        .replace("{repository}", &key.repository)
        .replace("{sha}", &key.commit_sha)
        .replace("{workflow}", &key.workflow)
}

fn strip_query(value: &str) -> String {
    value.split('?').next().unwrap_or(value).to_string()
}

fn sanitize_summary(value: &str) -> String {
    let mut summary = value.to_string();
    for name in ["authorization", "token", "password", "secret", "api_key"] {
        let lower = summary.to_ascii_lowercase();
        let Some(start) = lower.find(name) else {
            continue;
        };
        let end = summary[start..]
            .find(['\n', ' ', ','])
            .map(|offset| start + offset)
            .unwrap_or(summary.len());
        summary.replace_range(start..end, "[redacted]");
    }
    summary.truncate(MAX_SUMMARY_BYTES);
    summary
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiLifecycleState {
    Pending,
    Succeeded,
    ProductFailure,
    RetryScheduled,
    RetryExhausted,
    CorrelationFailed,
}

impl CiLifecycleState {
    fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::ProductFailure | Self::RetryExhausted | Self::CorrelationFailed
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiEvidence {
    pub observed_at: DateTime<Utc>,
    pub classification: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiLedgerEntry {
    pub key: CiCheckKey,
    pub parent_id: BeadId,
    pub check_id: BeadId,
    #[serde(default)]
    pub repair_id: Option<BeadId>,
    pub state: CiLifecycleState,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default)]
    pub next_retry_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub evidence: Vec<CiEvidence>,
    pub recorded_at: DateTime<Utc>,
}

struct CiLedger {
    path: PathBuf,
}

impl CiLedger {
    fn for_workspace(workspace: &Path, config: &PostPushCiConfig) -> Self {
        let root = config
            .state_dir
            .clone()
            .unwrap_or_else(|| workspace.join(".needle").join("ci"));
        let root = if root.is_absolute() {
            root
        } else {
            workspace.join(root)
        };
        Self {
            path: root.join(LEDGER_FILE),
        }
    }

    fn lock(&self) -> Result<File> {
        let lock_path = self.path.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create CI state directory {}", parent.display())
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("failed to open CI ledger lock {}", lock_path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("failed to lock CI ledger {}", lock_path.display()))?;
        Ok(file)
    }

    fn load_unlocked(&self) -> Result<HashMap<String, CiLedgerEntry>> {
        let mut entries = HashMap::new();
        if !self.path.exists() {
            return Ok(entries);
        }
        let file = File::open(&self.path)
            .with_context(|| format!("failed to open CI ledger {}", self.path.display()))?;
        for (line_number, line) in BufReader::new(file).lines().enumerate() {
            let line =
                line.with_context(|| format!("failed to read CI ledger line {line_number}"))?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: CiLedgerEntry = serde_json::from_str(&line).with_context(|| {
                format!(
                    "invalid CI ledger record at {}:{}",
                    self.path.display(),
                    line_number + 1
                )
            })?;
            entries.insert(entry.key.id(), entry);
        }
        Ok(entries)
    }

    fn append_unlocked(&self, entry: &CiLedgerEntry) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create CI state directory {}", parent.display())
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open CI ledger {}", self.path.display()))?;
        serde_json::to_writer(&mut file, entry).context("failed to serialize CI ledger record")?;
        file.write_all(b"\n")?;
        file.sync_data()
            .context("failed to durably flush CI ledger record")?;
        Ok(())
    }
}

/// Worker-free post-push reconciler.
pub struct CiCoordinator<'a> {
    store: &'a dyn BeadStore,
    config: PostPushCiConfig,
    telemetry: Option<Telemetry>,
}

impl<'a> CiCoordinator<'a> {
    pub fn new(
        store: &'a dyn BeadStore,
        config: PostPushCiConfig,
        telemetry: Option<Telemetry>,
    ) -> Self {
        Self {
            store,
            config,
            telemetry,
        }
    }

    /// Register exactly one check bead for a pushed key and make it block P.
    pub async fn register(
        &self,
        workspace: &Path,
        key: CiCheckKey,
        parent_id: &BeadId,
    ) -> Result<BeadId> {
        let ledger = CiLedger::for_workspace(workspace, &self.config);
        let _lock = ledger.lock()?;
        let mut states = ledger.load_unlocked()?;

        if let Some(entry) = states.get(&key.id()) {
            if entry.parent_id != *parent_id {
                bail!(
                    "CI key {} is already correlated to parent {}, not {}",
                    key.commit_sha,
                    entry.parent_id,
                    parent_id
                );
            }
            self.ensure_parent_open(parent_id).await?;
            self.ensure_parent_dependency(parent_id, &entry.check_id)
                .await?;
            self.emit_transition(entry, "register_idempotent", entry.state);
            return Ok(entry.check_id.clone());
        }

        self.ensure_parent_open(parent_id).await?;
        let all_beads = self.store.list_all().await?;
        let existing: Vec<(BeadId, CheckMarker)> = all_beads
            .iter()
            .filter_map(|bead| {
                parse_marker(bead.body.as_deref().unwrap_or_default(), CHECK_MARKER)
                    .and_then(|marker| serde_json::from_value::<CheckMarker>(marker).ok())
                    .filter(|marker| marker.key == key)
                    .map(|marker| (bead.id.clone(), marker))
            })
            .collect();
        if existing.len() > 1 {
            bail!("ambiguous duplicate CI check beads for {}", key.commit_sha);
        }
        if let Some((_, marker)) = existing.first() {
            if marker.parent_id != *parent_id {
                bail!(
                    "CI key {} is already correlated to parent {}, not {}",
                    key.commit_sha,
                    marker.parent_id,
                    parent_id
                );
            }
        }

        let check_id = if let Some((id, _)) = existing.first() {
            id.clone()
        } else {
            let body = format!(
                "{}\n\nAuthoritative Forgejo/Argo verification for {} at {}.",
                marker_json(
                    CHECK_MARKER,
                    &CheckMarker {
                        key: key.clone(),
                        parent_id: parent_id.clone(),
                    }
                )?,
                key.repository,
                key.commit_sha,
            );
            let id = self
                .store
                .create_bead(
                    &format!(
                        "CI verification: {} @ {}",
                        key.workflow,
                        &key.commit_sha[..key.commit_sha.len().min(12)]
                    ),
                    &body,
                    &[CHECK_LABEL],
                )
                .await?;
            self.emit_transition_values(parent_id, &id, &key, "check_created", "pending");
            id
        };

        self.ensure_parent_dependency(parent_id, &check_id).await?;

        let entry = CiLedgerEntry {
            key: key.clone(),
            parent_id: parent_id.clone(),
            check_id: check_id.clone(),
            repair_id: None,
            state: CiLifecycleState::Pending,
            retry_count: 0,
            next_retry_at: None,
            evidence: Vec::new(),
            recorded_at: Utc::now(),
        };
        ledger.append_unlocked(&entry)?;
        states.insert(key.id(), entry.clone());
        self.emit_transition(&entry, "registered", entry.state);
        Ok(check_id)
    }

    async fn ensure_parent_open(&self, parent_id: &BeadId) -> Result<()> {
        let parent = self.store.show(parent_id).await?;
        if parent.status.is_done() {
            self.store.reopen(parent_id).await?;
        }
        Ok(())
    }

    async fn ensure_parent_dependency(
        &self,
        parent_id: &BeadId,
        blocker_id: &BeadId,
    ) -> Result<()> {
        let parent = self.store.show(parent_id).await?;
        if !parent
            .dependencies
            .iter()
            .any(|dependency| dependency.id == *blocker_id)
        {
            // The first argument is the blocker; the second is the blocked
            // parent.  This direction is asserted in the state-machine tests.
            self.store.add_dependency(blocker_id, parent_id).await?;
        }
        Ok(())
    }

    /// Rehydrate ledger records from check bead markers after a process or
    /// machine restart.  This closes the create-bead/ledger-write crash gap.
    pub async fn recover(&self, workspace: &Path) -> Result<usize> {
        let ledger = CiLedger::for_workspace(workspace, &self.config);
        let _lock = ledger.lock()?;
        let mut states = ledger.load_unlocked()?;
        let mut recovered = 0;
        for bead in self.store.list_all().await? {
            let Some(value) = parse_marker(bead.body.as_deref().unwrap_or_default(), CHECK_MARKER)
            else {
                continue;
            };
            let Ok(marker) = serde_json::from_value::<CheckMarker>(value) else {
                continue;
            };
            if states.contains_key(&marker.key.id()) {
                continue;
            }
            let entry = CiLedgerEntry {
                key: marker.key.clone(),
                parent_id: marker.parent_id,
                check_id: bead.id,
                repair_id: None,
                state: CiLifecycleState::Pending,
                retry_count: 0,
                next_retry_at: None,
                evidence: Vec::new(),
                recorded_at: Utc::now(),
            };
            ledger.append_unlocked(&entry)?;
            self.emit_transition(&entry, "recovered", entry.state);
            states.insert(marker.key.id(), entry);
            recovered += 1;
        }
        Ok(recovered)
    }

    /// Reconcile all non-terminal checks whose retry deadline has arrived.
    pub async fn reconcile_once(
        &self,
        workspace: &Path,
        source: &dyn CiResultSource,
    ) -> Result<Vec<ReconcileOutcome>> {
        self.recover(workspace).await?;
        let ledger = CiLedger::for_workspace(workspace, &self.config);
        let states = {
            let _lock = ledger.lock()?;
            ledger.load_unlocked()?
        };
        let mut outcomes = Vec::new();
        for entry in states.into_values() {
            if entry.state.terminal()
                || entry
                    .next_retry_at
                    .is_some_and(|deadline| deadline > Utc::now())
            {
                continue;
            }
            let observation = source.poll(&entry.key).await?;
            outcomes.push(
                self.reconcile_observation(workspace, entry, observation)
                    .await?,
            );
        }
        Ok(outcomes)
    }

    /// Apply a webhook/event result.  The full key comparison makes duplicate
    /// and out-of-order events harmless: an old SHA can never advance a newer
    /// check, even when both belong to the same parent bead.
    pub async fn reconcile_event(
        &self,
        workspace: &Path,
        key: &CiCheckKey,
        observation: CiObservation,
    ) -> Result<ReconcileOutcome> {
        let ledger = CiLedger::for_workspace(workspace, &self.config);
        let states = {
            let _lock = ledger.lock()?;
            ledger.load_unlocked()?
        };
        let Some(entry) = states.get(&key.id()).cloned() else {
            self.emit_transition_values(
                &BeadId::from("unknown"),
                &BeadId::from("unknown"),
                key,
                "out_of_order_ignored",
                "unknown_key",
            );
            return Ok(ReconcileOutcome::IgnoredOutOfOrder);
        };
        if entry.state.terminal() {
            self.emit_transition(&entry, "duplicate_ignored", entry.state);
            return Ok(ReconcileOutcome::DuplicateIgnored);
        }
        self.reconcile_observation(workspace, entry, observation)
            .await
    }

    async fn reconcile_observation(
        &self,
        workspace: &Path,
        mut entry: CiLedgerEntry,
        observation: CiObservation,
    ) -> Result<ReconcileOutcome> {
        if entry.state.terminal() {
            self.emit_transition(&entry, "duplicate_ignored", entry.state);
            return Ok(ReconcileOutcome::DuplicateIgnored);
        }
        if !matches!(observation, CiObservation::Pending { .. }) {
            if let Some(event_id) = observation_event_id(&observation) {
                if entry
                    .evidence
                    .iter()
                    .any(|evidence| evidence.event_id.as_deref() == Some(event_id))
                {
                    self.emit_transition(&entry, "duplicate_ignored", entry.state);
                    return Ok(ReconcileOutcome::DuplicateIgnored);
                }
            }
        }

        let now = Utc::now();
        let observation = match observation {
            CiObservation::Pending {
                run_reference,
                event_id,
                summary,
            } if now
                >= entry
                    .recorded_at
                    .checked_add_signed(chrono::Duration::seconds(
                        self.timeout_secs(&entry.key) as i64
                    ))
                    .unwrap_or(now) =>
            {
                CiObservation::Failure {
                    class: CiFailureClass::Timeout,
                    run_reference,
                    log_reference: None,
                    event_id,
                    summary: format!(
                        "authoritative CI run exceeded its configured timeout: {}",
                        sanitize_summary(&summary)
                    ),
                }
            }
            observation => observation,
        };
        let evidence = observation_evidence(&observation, now);
        match observation {
            CiObservation::Pending { .. } => {
                entry.state = CiLifecycleState::Pending;
                entry.next_retry_at = Some(
                    now + chrono::Duration::seconds(self.config.poll_interval_secs.max(1) as i64),
                );
                self.persist(workspace, &entry, "pending").await?;
                Ok(ReconcileOutcome::Pending)
            }
            CiObservation::Success { .. } => {
                entry.evidence.push(evidence);
                entry.next_retry_at = None;
                // Persist before side effects: a restart after this point can
                // safely repeat close operations, which are idempotent.
                entry.state = CiLifecycleState::Pending;
                self.persist(workspace, &entry, "success_observed").await?;
                self.close_if_open(
                    &entry.check_id,
                    &format!("authoritative CI passed for {}", entry.key.commit_sha),
                )
                .await?;
                let parent = self.store.show(&entry.parent_id).await?;
                if !parent.status.is_done() && !self.has_unfinished_blockers(&parent).await? {
                    self.close_if_open(&entry.parent_id, "authoritative post-push CI passed")
                        .await?;
                }
                entry.state = CiLifecycleState::Succeeded;
                self.persist(workspace, &entry, "succeeded").await?;
                Ok(ReconcileOutcome::Succeeded)
            }
            CiObservation::Failure {
                class: CiFailureClass::Product,
                ..
            } => {
                entry.evidence.push(evidence);
                entry.next_retry_at = None;
                self.persist(workspace, &entry, "product_failure_observed")
                    .await?;
                self.close_if_open(
                    &entry.check_id,
                    "authoritative CI failed: product/test failure",
                )
                .await?;
                let repair_id = self.ensure_repair(workspace, &entry).await?;
                entry.repair_id = Some(repair_id);
                entry.state = CiLifecycleState::ProductFailure;
                self.persist(workspace, &entry, "repair_created").await?;
                Ok(ReconcileOutcome::RepairCreated)
            }
            CiObservation::Failure { .. } => {
                entry.evidence.push(evidence);
                entry.retry_count = entry.retry_count.saturating_add(1);
                if entry.retry_count > self.max_retries(&entry.key) {
                    entry.state = CiLifecycleState::RetryExhausted;
                    entry.next_retry_at = None;
                    self.persist(workspace, &entry, "retry_exhausted").await?;
                    return Ok(ReconcileOutcome::RetryExhausted);
                }
                entry.state = CiLifecycleState::RetryScheduled;
                entry.next_retry_at = Some(
                    now + chrono::Duration::seconds(self.config.poll_interval_secs.max(1) as i64),
                );
                self.persist(workspace, &entry, "retry_scheduled").await?;
                self.emit_transition(&entry, "retry_scheduled", entry.state);
                Ok(ReconcileOutcome::RetryScheduled)
            }
        }
    }

    async fn ensure_repair(&self, workspace: &Path, entry: &CiLedgerEntry) -> Result<BeadId> {
        let ledger = CiLedger::for_workspace(workspace, &self.config);
        let _lock = ledger.lock()?;
        let all_beads = self.store.list_all().await?;
        let mut matches = Vec::new();
        for bead in &all_beads {
            if let Some(value) =
                parse_marker(bead.body.as_deref().unwrap_or_default(), REPAIR_MARKER)
            {
                if let Ok(marker) = serde_json::from_value::<RepairMarker>(value) {
                    if marker.parent_id == entry.parent_id && marker.failed_key == entry.key {
                        matches.push(bead.id.clone());
                    }
                }
            }
        }
        if matches.len() > 1 {
            bail!(
                "ambiguous duplicate CI repair beads for {}",
                entry.key.commit_sha
            );
        }
        if let Some(id) = matches.into_iter().next() {
            let parent = self.store.show(&entry.parent_id).await?;
            if parent.status.is_done() {
                self.store.reopen(&entry.parent_id).await?;
            }
            if !parent
                .dependencies
                .iter()
                .any(|dependency| dependency.id == id)
            {
                self.store.add_dependency(&id, &entry.parent_id).await?;
            }
            return Ok(id);
        }

        let body = format!(
            "{}\n\nRepair the product/test failure reported by authoritative CI for {}.",
            marker_json(
                REPAIR_MARKER,
                &RepairMarker {
                    parent_id: entry.parent_id.clone(),
                    failed_key: entry.key.clone(),
                    check_id: entry.check_id.clone(),
                }
            )?,
            entry.key.commit_sha,
        );
        let id = self
            .store
            .create_bead(
                &format!(
                    "Repair CI failure: {}",
                    &entry.key.commit_sha[..entry.key.commit_sha.len().min(12)]
                ),
                &body,
                &[REPAIR_LABEL],
            )
            .await?;
        self.store.add_dependency(&id, &entry.parent_id).await?;
        Ok(id)
    }

    async fn has_unfinished_blockers(&self, parent: &Bead) -> Result<bool> {
        for dependency in &parent.dependencies {
            // Dependency projections are often stale (notably after closing
            // the check bead immediately above), so always re-read the
            // authoritative blocker record.  A failed read is conservatively
            // treated as unfinished rather than releasing the parent.
            let status = match self.store.show(&dependency.id).await {
                Ok(bead) => bead.status,
                Err(_) => return Ok(true),
            };
            if !status.is_done() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn close_if_open(&self, id: &BeadId, reason: &str) -> Result<()> {
        let bead = self.store.show(id).await?;
        if !bead.status.is_done() {
            self.store.close(id, reason).await?;
        }
        Ok(())
    }

    fn timeout_secs(&self, key: &CiCheckKey) -> u64 {
        self.config
            .repositories
            .get(&key.repository)
            .and_then(|config| config.timeout_secs)
            .unwrap_or(self.config.timeout_secs)
    }

    fn max_retries(&self, key: &CiCheckKey) -> u32 {
        self.config
            .repositories
            .get(&key.repository)
            .and_then(|config| config.max_retries)
            .unwrap_or(self.config.max_retries)
    }

    async fn persist(
        &self,
        workspace: &Path,
        entry: &CiLedgerEntry,
        transition: &str,
    ) -> Result<()> {
        let ledger = CiLedger::for_workspace(workspace, &self.config);
        let _lock = ledger.lock()?;
        ledger.append_unlocked(entry)?;
        self.emit_transition(entry, transition, entry.state);
        Ok(())
    }

    fn emit_transition(&self, entry: &CiLedgerEntry, transition: &str, state: CiLifecycleState) {
        self.emit_transition_values(
            &entry.parent_id,
            &entry.check_id,
            &entry.key,
            transition,
            &format!("{state:?}"),
        );
    }

    fn emit_transition_values(
        &self,
        parent_id: &BeadId,
        check_id: &BeadId,
        key: &CiCheckKey,
        transition: &str,
        state: &str,
    ) {
        if let Some(telemetry) = &self.telemetry {
            let _ = telemetry.emit_try_lock(EventKind::Log {
                phase: "ci.reconcile".to_string(),
                level: "info".to_string(),
                bead_id: Some(parent_id.clone()),
                context: serde_json::json!({
                    "transition": transition,
                    "state": state,
                    "parent_id": parent_id,
                    "check_id": check_id,
                    "repository": key.repository,
                    "commit_sha": key.commit_sha,
                    "workflow": key.workflow,
                    "correlation_id": format!("{}:{}:{}", key.repository, key.commit_sha, key.workflow),
                }),
            });
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileOutcome {
    Pending,
    Succeeded,
    RepairCreated,
    RetryScheduled,
    RetryExhausted,
    DuplicateIgnored,
    IgnoredOutOfOrder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CheckMarker {
    key: CiCheckKey,
    parent_id: BeadId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RepairMarker {
    parent_id: BeadId,
    failed_key: CiCheckKey,
    check_id: BeadId,
}

fn marker_json<T: Serialize>(prefix: &str, marker: &T) -> Result<String> {
    Ok(format!(
        "<!-- {prefix} {} -->",
        serde_json::to_string(marker)?
    ))
}

fn parse_marker(body: &str, prefix: &str) -> Option<serde_json::Value> {
    let marker = format!("<!-- {prefix}");
    let start = body.find(&marker)? + marker.len();
    let remainder = body.get(start..)?.trim_start();
    let end = remainder.find("-->")?;
    serde_json::from_str(remainder[..end].trim()).ok()
}

fn observation_evidence(observation: &CiObservation, observed_at: DateTime<Utc>) -> CiEvidence {
    match observation {
        CiObservation::Pending {
            run_reference,
            event_id,
            summary,
        } => CiEvidence {
            observed_at,
            classification: "pending".to_string(),
            summary: sanitize_summary(summary),
            run_reference: run_reference.as_deref().map(strip_query),
            log_reference: None,
            event_id: event_id.clone(),
        },
        CiObservation::Success {
            run_reference,
            log_reference,
            event_id,
            summary,
        } => CiEvidence {
            observed_at,
            classification: "success".to_string(),
            summary: sanitize_summary(summary),
            run_reference: run_reference.as_deref().map(strip_query),
            log_reference: log_reference.as_deref().map(strip_query),
            event_id: event_id.clone(),
        },
        CiObservation::Failure {
            class,
            run_reference,
            log_reference,
            event_id,
            summary,
        } => CiEvidence {
            observed_at,
            classification: format!("{class:?}").to_ascii_lowercase(),
            summary: sanitize_summary(summary),
            run_reference: run_reference.as_deref().map(strip_query),
            log_reference: log_reference.as_deref().map(strip_query),
            event_id: event_id.clone(),
        },
    }
}

fn observation_event_id(observation: &CiObservation) -> Option<&str> {
    match observation {
        CiObservation::Pending { event_id, .. }
        | CiObservation::Success { event_id, .. }
        | CiObservation::Failure { event_id, .. } => event_id.as_deref(),
    }
}

/// Return the normalized origin identity used in idempotency keys and config.
pub fn normalize_repository(remote: &str) -> String {
    let mut value = remote.trim().trim_end_matches('/').to_string();
    if let Some(rest) = value.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            value = format!("{host}/{path}");
        }
    }
    if let Some(rest) = value.strip_prefix("ssh://git@") {
        value = format!("ssh://{rest}");
    }
    if let Some(rest) = value.strip_prefix("https://") {
        value = format!("https://{}", rest.trim_start_matches("git@"));
    }
    if value.ends_with(".git") {
        value.truncate(value.len() - 4);
    }
    value
}

/// Read exactly one `Bead-Id` trailer from a commit.
pub async fn correlate_commit(
    workspace: &Path,
    commit_sha: &str,
) -> Result<BeadId, CorrelationError> {
    let message = git_output(workspace, &["show", "-s", "--format=%B", commit_sha])
        .await
        .map_err(|_| CorrelationError::MissingTrailer {
            commit_sha: commit_sha.to_string(),
        })?;
    let parents: Vec<BeadId> = message
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("Bead-Id"))
        .map(|(_, value)| BeadId::from(value.trim()))
        .filter(|id| !id.as_ref().is_empty())
        .collect();
    match parents.as_slice() {
        [] => Err(CorrelationError::MissingTrailer {
            commit_sha: commit_sha.to_string(),
        }),
        [parent] => Ok(parent.clone()),
        _ => Err(CorrelationError::AmbiguousTrailers {
            commit_sha: commit_sha.to_string(),
            parents,
        }),
    }
}

/// Register the commit produced by a worker after it has pushed.  A missing or
/// ambiguous trailer is a safe no-op with the parent reopened; it never guesses
/// which implementation bead should receive the CI check.
pub async fn register_post_push_commit(
    config: &PostPushCiConfig,
    store: &dyn BeadStore,
    telemetry: Option<Telemetry>,
    workspace: &Path,
    parent_id: &BeadId,
    pre_dispatch_head: Option<&str>,
) -> Result<RegistrationResult> {
    if !config.enabled {
        return Ok(RegistrationResult::Disabled);
    }
    let Some(pre_dispatch_head) = pre_dispatch_head else {
        // Without the worker's baseline we cannot prove that HEAD was produced
        // by this dispatch in a shared checkout.  Safe non-correlation is
        // preferable to attaching another worker's pushed commit to P.
        return Ok(RegistrationResult::NoPushedCommit);
    };
    let head = git_output(workspace, &["rev-parse", "HEAD"]).await?;
    if pre_dispatch_head.trim() == head {
        return Ok(RegistrationResult::NoPushedCommit);
    }
    if git_output(workspace, &["merge-base", "--is-ancestor", &head, "@{u}"])
        .await
        .is_err()
    {
        return Ok(RegistrationResult::NoPushedCommit);
    }
    let remote = git_output(workspace, &["remote", "get-url", "origin"]).await?;
    let parent_from_commit = match correlate_commit(workspace, &head).await {
        Ok(parent) => parent,
        Err(error) => {
            reopen_parent(store, parent_id).await;
            emit_correlation_failure(telemetry.as_ref(), parent_id, &head, &error);
            return Ok(RegistrationResult::CorrelationFailed(error));
        }
    };
    if parent_from_commit != *parent_id {
        reopen_parent(store, parent_id).await;
        let error = CorrelationError::TrailerMismatch {
            expected: parent_id.clone(),
            found: parent_from_commit,
        };
        emit_correlation_failure(telemetry.as_ref(), parent_id, &head, &error);
        return Ok(RegistrationResult::CorrelationFailed(error));
    }
    let repository = normalize_repository(&remote);
    let workflow = config
        .repositories
        .get(&repository)
        .and_then(|repo| repo.workflow.clone())
        .unwrap_or_else(|| config.default_workflow.clone());
    let key = CiCheckKey {
        repository,
        commit_sha: head,
        workflow,
    };
    let coordinator = CiCoordinator::new(store, config.clone(), telemetry);
    let check_id = coordinator
        .register(workspace, key.clone(), parent_id)
        .await?;
    Ok(RegistrationResult::Registered { key, check_id })
}

async fn reopen_parent(store: &dyn BeadStore, parent_id: &BeadId) {
    if let Ok(parent) = store.show(parent_id).await {
        if parent.status.is_done() {
            let _ = store.reopen(parent_id).await;
        }
    }
}

fn emit_correlation_failure(
    telemetry: Option<&Telemetry>,
    parent_id: &BeadId,
    commit_sha: &str,
    error: &CorrelationError,
) {
    if let Some(telemetry) = telemetry {
        let _ = telemetry.emit_try_lock(EventKind::Log {
            phase: "ci.reconcile".to_string(),
            level: "warn".to_string(),
            bead_id: Some(parent_id.clone()),
            context: serde_json::json!({
                "transition": "correlation_failed",
                "correlation_id": format!("parent:{}:commit:{}", parent_id, commit_sha),
                "parent_id": parent_id,
                "commit_sha": commit_sha,
                "error": error.to_string(),
            }),
        });
    }
}

async fn git_output(workspace: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("failed to run git {:?}", args))?;
    if !output.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::{Filters, RepairReport};
    use crate::types::{Bead, BeadStatus, ClaimResult};
    use std::sync::{Arc, Mutex};

    fn key() -> CiCheckKey {
        CiCheckKey {
            repository: "https://forgejo.example/acme/app".to_string(),
            commit_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
            workflow: "needle-ci".to_string(),
        }
    }

    fn config(dir: &Path) -> PostPushCiConfig {
        PostPushCiConfig {
            enabled: true,
            result_url_template: Some("https://argo.example/runs/{sha}/{workflow}".to_string()),
            state_dir: Some(dir.to_path_buf()),
            ..PostPushCiConfig::default()
        }
    }

    fn bead(id: &str, status: BeadStatus) -> Bead {
        Bead {
            id: BeadId::from(id),
            title: id.to_string(),
            body: Some("body".to_string()),
            priority: 1,
            status,
            assignee: None,
            labels: Vec::new(),
            workspace: PathBuf::from("/tmp"),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[derive(Clone)]
    struct MockStore {
        beads: Arc<Mutex<Vec<Bead>>>,
        actions: Arc<Mutex<Vec<String>>>,
        next: Arc<Mutex<u32>>,
    }

    impl MockStore {
        fn new(parent_status: BeadStatus) -> Self {
            Self {
                beads: Arc::new(Mutex::new(vec![bead("parent", parent_status)])),
                actions: Arc::new(Mutex::new(Vec::new())),
                next: Arc::new(Mutex::new(0)),
            }
        }
        fn action(&self) -> Vec<String> {
            self.actions.lock().unwrap().clone()
        }
        fn statuses(&self) -> Vec<(String, BeadStatus)> {
            self.beads
                .lock()
                .unwrap()
                .iter()
                .map(|bead| (bead.id.to_string(), bead.status.clone()))
                .collect()
        }
    }

    #[async_trait]
    impl BeadStore for MockStore {
        async fn ready(&self, _: &Filters) -> Result<Vec<Bead>> {
            Ok(Vec::new())
        }
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().unwrap().clone())
        }
        async fn show(&self, id: &BeadId) -> Result<Bead> {
            self.beads
                .lock()
                .unwrap()
                .iter()
                .find(|bead| bead.id == *id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing bead {id}"))
        }
        async fn claim(&self, _: &BeadId, _: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "test".to_string(),
            })
        }
        async fn claim_auto(&self, _: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "test".to_string(),
            })
        }
        async fn release(&self, id: &BeadId) -> Result<()> {
            self.actions.lock().unwrap().push(format!("release:{id}"));
            Ok(())
        }
        async fn block(&self, _: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn clear_assignee(&self, _: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
        async fn reopen(&self, id: &BeadId) -> Result<()> {
            self.actions.lock().unwrap().push(format!("reopen:{id}"));
            if let Some(bead) = self
                .beads
                .lock()
                .unwrap()
                .iter_mut()
                .find(|bead| bead.id == *id)
            {
                bead.status = BeadStatus::Open;
            }
            Ok(())
        }
        async fn close(&self, id: &BeadId, _: &str) -> Result<()> {
            self.actions.lock().unwrap().push(format!("close:{id}"));
            if let Some(bead) = self
                .beads
                .lock()
                .unwrap()
                .iter_mut()
                .find(|bead| bead.id == *id)
            {
                bead.status = BeadStatus::Closed;
            }
            Ok(())
        }
        async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
            Ok(self.show(id).await?.labels)
        }
        async fn add_label(&self, _: &BeadId, _: &str) -> Result<()> {
            Ok(())
        }
        async fn remove_label(&self, _: &BeadId, _: &str) -> Result<()> {
            Ok(())
        }
        async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
            let mut next = self.next.lock().unwrap();
            *next += 1;
            let id = BeadId::from(format!("ci-{next}"));
            let mut created = bead(id.as_ref(), BeadStatus::Open);
            created.title = title.to_string();
            created.body = Some(body.to_string());
            created.labels = labels.iter().map(|label| (*label).to_string()).collect();
            self.beads.lock().unwrap().push(created);
            Ok(id)
        }
        async fn add_dependency(&self, blocker: &BeadId, blocked: &BeadId) -> Result<()> {
            self.actions
                .lock()
                .unwrap()
                .push(format!("dep:{blocker}->{blocked}"));
            if let Some(parent) = self
                .beads
                .lock()
                .unwrap()
                .iter_mut()
                .find(|bead| bead.id == *blocked)
            {
                parent.dependencies.push(crate::types::BrDependency {
                    id: blocker.clone(),
                    title: String::new(),
                    status: "open".to_string(),
                    priority: 1,
                    dependency_type: "blocks".to_string(),
                });
            }
            Ok(())
        }
        async fn remove_dependency(&self, _: &BeadId, _: &BeadId) -> Result<()> {
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
        fn has_valid_store(&self) -> bool {
            true
        }
    }

    #[test]
    fn repository_normalization_and_marker_parsing_are_stable() {
        assert_eq!(
            normalize_repository("git@forgejo.example:acme/app.git"),
            "forgejo.example/acme/app"
        );
        let marker = marker_json(
            CHECK_MARKER,
            &CheckMarker {
                key: key(),
                parent_id: BeadId::from("parent"),
            },
        )
        .unwrap();
        let parsed = parse_marker(&marker, CHECK_MARKER).unwrap();
        assert_eq!(parsed["parent_id"], "parent");
    }

    #[test]
    fn authoritative_statuses_are_classified_without_credentials() {
        let value = serde_json::json!({"status":{"phase":"Failed","message":"token=secret cargo test"},"metadata":{"name":"run-1"}});
        let result =
            parse_authoritative_result(&value, Some("https://argo/run?token=secret".to_string()));
        match result {
            CiObservation::Failure {
                class: CiFailureClass::Product,
                run_reference,
                summary,
                ..
            } => {
                assert_eq!(run_reference.as_deref(), Some("https://argo/run"));
                assert!(!summary.contains("secret"));
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn commit_correlation_requires_one_unambiguous_trailer() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap()
        };
        assert!(run(&["init", "-q"]).status.success());
        assert!(run(&["config", "user.email", "test@example.com"])
            .status
            .success());
        assert!(run(&["config", "user.name", "test"]).status.success());

        std::fs::write(dir.path().join("README.md"), "one\n").unwrap();
        assert!(run(&["add", "README.md"]).status.success());
        assert!(run(&["commit", "-q", "-m", "feat: one\n\nBead-Id: parent"])
            .status
            .success());
        let sha = String::from_utf8(run(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(
            correlate_commit(dir.path(), &sha).await.unwrap(),
            BeadId::from("parent")
        );

        std::fs::write(dir.path().join("README.md"), "two\n").unwrap();
        assert!(run(&["add", "README.md"]).status.success());
        assert!(run(&[
            "commit",
            "-q",
            "-m",
            "feat: two\n\nBead-Id: first\nBead-Id: second",
        ])
        .status
        .success());
        let ambiguous = String::from_utf8(run(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        assert!(matches!(
            correlate_commit(dir.path(), &ambiguous).await,
            Err(CorrelationError::AmbiguousTrailers { .. })
        ));

        std::fs::write(dir.path().join("README.md"), "three\n").unwrap();
        assert!(run(&["add", "README.md"]).status.success());
        assert!(run(&["commit", "-q", "-m", "feat: three"]).status.success());
        let missing = String::from_utf8(run(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        assert!(matches!(
            correlate_commit(dir.path(), &missing).await,
            Err(CorrelationError::MissingTrailer { .. })
        ));
    }

    #[tokio::test]
    async fn check_is_idempotent_and_dependency_direction_blocks_parent() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(BeadStatus::Closed);
        let coordinator = CiCoordinator::new(&store, config(dir.path()), None);
        let first = coordinator
            .register(dir.path(), key(), &BeadId::from("parent"))
            .await
            .unwrap();
        let second = coordinator
            .register(dir.path(), key(), &BeadId::from("parent"))
            .await
            .unwrap();
        assert_eq!(first, second);
        let actions = store.action();
        assert!(actions.iter().any(|action| action == "dep:ci-1->parent"));
        assert_eq!(
            store
                .beads
                .lock()
                .unwrap()
                .iter()
                .filter(|bead| bead.labels.contains(&CHECK_LABEL.to_string()))
                .count(),
            1
        );
        assert!(store
            .statuses()
            .iter()
            .any(|(id, status)| id == "parent" && *status == BeadStatus::Open));
    }

    #[tokio::test]
    async fn success_closes_check_and_parent_only_when_unblocked() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(BeadStatus::Open);
        let coordinator = CiCoordinator::new(&store, config(dir.path()), None);
        let check = coordinator
            .register(dir.path(), key(), &BeadId::from("parent"))
            .await
            .unwrap();
        let result = coordinator
            .reconcile_event(
                dir.path(),
                &key(),
                CiObservation::Success {
                    run_reference: Some("https://run/1".to_string()),
                    log_reference: Some("https://run/1/log".to_string()),
                    event_id: Some("1".to_string()),
                    summary: "all passed".to_string(),
                },
            )
            .await
            .unwrap();
        assert_eq!(result, ReconcileOutcome::Succeeded);
        assert!(store
            .action()
            .iter()
            .any(|action| action == &format!("close:{check}")));
        assert!(store.action().iter().any(|action| action == "close:parent"));
    }

    #[tokio::test]
    async fn success_leaves_parent_open_when_another_blocker_is_unfinished() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(BeadStatus::Open);
        store
            .beads
            .lock()
            .unwrap()
            .push(bead("other", BeadStatus::Open));
        store
            .beads
            .lock()
            .unwrap()
            .iter_mut()
            .find(|bead| bead.id == BeadId::from("parent"))
            .unwrap()
            .dependencies
            .push(crate::types::BrDependency {
                id: BeadId::from("other"),
                title: "other blocker".to_string(),
                status: "open".to_string(),
                priority: 1,
                dependency_type: "blocks".to_string(),
            });
        let coordinator = CiCoordinator::new(&store, config(dir.path()), None);
        let check = coordinator
            .register(dir.path(), key(), &BeadId::from("parent"))
            .await
            .unwrap();
        coordinator
            .reconcile_event(
                dir.path(),
                &key(),
                CiObservation::Success {
                    run_reference: None,
                    log_reference: None,
                    event_id: Some("success-1".to_string()),
                    summary: "passed".to_string(),
                },
            )
            .await
            .unwrap();
        assert!(store
            .action()
            .iter()
            .any(|action| action == &format!("close:{check}")));
        assert!(!store.action().iter().any(|action| action == "close:parent"));
        assert_eq!(
            store
                .statuses()
                .into_iter()
                .find(|(id, _)| id == "parent")
                .map(|(_, status)| status),
            Some(BeadStatus::Open)
        );
    }

    #[tokio::test]
    async fn product_failure_creates_one_repair_and_retry_never_creates_code_defect() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(BeadStatus::Open);
        let coordinator = CiCoordinator::new(&store, config(dir.path()), None);
        coordinator
            .register(dir.path(), key(), &BeadId::from("parent"))
            .await
            .unwrap();
        let failure = CiObservation::Failure {
            class: CiFailureClass::Product,
            run_reference: Some("https://run/1".to_string()),
            log_reference: Some("https://run/1/log".to_string()),
            event_id: Some("1".to_string()),
            summary: "test failed".to_string(),
        };
        assert_eq!(
            coordinator
                .reconcile_event(dir.path(), &key(), failure.clone())
                .await
                .unwrap(),
            ReconcileOutcome::RepairCreated
        );
        assert_eq!(
            coordinator
                .reconcile_event(dir.path(), &key(), failure)
                .await
                .unwrap(),
            ReconcileOutcome::DuplicateIgnored
        );
        assert_eq!(
            store
                .beads
                .lock()
                .unwrap()
                .iter()
                .filter(|bead| bead.labels.contains(&REPAIR_LABEL.to_string()))
                .count(),
            1
        );
        let evidence = std::fs::read_to_string(dir.path().join(LEDGER_FILE)).unwrap();
        assert!(evidence.contains("https://run/1"));
        assert!(evidence.contains("test failed"));
        assert!(store
            .action()
            .iter()
            .any(|action| action.starts_with("dep:ci-")));

        let repaired_key = CiCheckKey {
            commit_sha: "abcdefabcdefabcdefabcdefabcdefabcdefabcd".to_string(),
            ..key()
        };
        let replacement_check = coordinator
            .register(dir.path(), repaired_key, &BeadId::from("parent"))
            .await
            .unwrap();
        assert_eq!(replacement_check, BeadId::from("ci-3"));
        assert_eq!(
            store
                .beads
                .lock()
                .unwrap()
                .iter()
                .filter(|bead| bead.labels.contains(&CHECK_LABEL.to_string()))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn infrastructure_timeout_and_out_of_order_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.max_retries = 2;
        let store = MockStore::new(BeadStatus::Open);
        let coordinator = CiCoordinator::new(&store, cfg, None);
        coordinator
            .register(dir.path(), key(), &BeadId::from("parent"))
            .await
            .unwrap();
        let other = CiCheckKey {
            commit_sha: "fedcba9876543210fedcba9876543210fedcba98".to_string(),
            ..key()
        };
        assert_eq!(
            coordinator
                .reconcile_event(
                    dir.path(),
                    &other,
                    CiObservation::Success {
                        run_reference: None,
                        log_reference: None,
                        event_id: None,
                        summary: "old".to_string()
                    }
                )
                .await
                .unwrap(),
            ReconcileOutcome::IgnoredOutOfOrder
        );
        let transient = CiObservation::Failure {
            class: CiFailureClass::Infrastructure,
            run_reference: None,
            log_reference: None,
            event_id: Some("infra-1".to_string()),
            summary: "runner unavailable".to_string(),
        };
        assert_eq!(
            coordinator
                .reconcile_event(dir.path(), &key(), transient.clone())
                .await
                .unwrap(),
            ReconcileOutcome::RetryScheduled
        );
        assert_eq!(
            coordinator
                .reconcile_event(dir.path(), &key(), transient)
                .await
                .unwrap(),
            ReconcileOutcome::DuplicateIgnored
        );
        assert_eq!(
            coordinator
                .reconcile_event(
                    dir.path(),
                    &key(),
                    CiObservation::Failure {
                        class: CiFailureClass::Infrastructure,
                        run_reference: None,
                        log_reference: None,
                        event_id: Some("infra-2".to_string()),
                        summary: "runner unavailable again".to_string(),
                    },
                )
                .await
                .unwrap(),
            ReconcileOutcome::RetryScheduled
        );
        assert_eq!(
            coordinator
                .reconcile_event(
                    dir.path(),
                    &key(),
                    CiObservation::Failure {
                        class: CiFailureClass::Timeout,
                        run_reference: None,
                        log_reference: None,
                        event_id: Some("timeout-1".to_string()),
                        summary: "timeout".to_string()
                    }
                )
                .await
                .unwrap(),
            ReconcileOutcome::RetryExhausted
        );
        assert_eq!(
            store
                .beads
                .lock()
                .unwrap()
                .iter()
                .filter(|bead| bead.labels.contains(&REPAIR_LABEL.to_string()))
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn restart_recovery_rehydrates_a_created_check() {
        let dir = tempfile::tempdir().unwrap();
        let store = MockStore::new(BeadStatus::Open);
        let cfg = config(dir.path());
        let marker = marker_json(
            CHECK_MARKER,
            &CheckMarker {
                key: key(),
                parent_id: BeadId::from("parent"),
            },
        )
        .unwrap();
        let id = store
            .create_bead("existing", &marker, &[CHECK_LABEL])
            .await
            .unwrap();
        let coordinator = CiCoordinator::new(&store, cfg, None);
        assert_eq!(coordinator.recover(dir.path()).await.unwrap(), 1);
        assert_eq!(coordinator.recover(dir.path()).await.unwrap(), 0);
        let result = coordinator
            .reconcile_event(
                dir.path(),
                &key(),
                CiObservation::Success {
                    run_reference: None,
                    log_reference: None,
                    event_id: None,
                    summary: "ok".to_string(),
                },
            )
            .await
            .unwrap();
        assert_eq!(result, ReconcileOutcome::Succeeded);
        assert!(store
            .action()
            .iter()
            .any(|action| action == &format!("close:{id}")));
    }
}
