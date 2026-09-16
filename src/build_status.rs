//! Build status for a workspace, as reported by its CI workflow.
//!
//! The circuit breaker asks one question before a claim lands: is the newest
//! run of this workspace's CI template green? A gate that is permanently red
//! carries no signal, so workers learn to route around it — the fix is to stop
//! claiming work in a workspace whose main branch is failing, and to lift the
//! stop the moment a run Succeeds.
//!
//! The authoritative source is the Argo Workflows API on iad-ci, reached
//! through the credential-free read-only kubectl proxy endpoint documented in
//! `CLAUDE.md`. Workflows are matched on `spec.workflowTemplateRef.name`,
//! which the `needle-ci` template sets on every run it starts (labels are not
//! reliable here — most workflows in the namespace carry no template label).
//! The run's `status.phase` maps onto [`BuildStatus`]:
//!
//! | phase               | status     | claim behaviour            |
//! |---------------------|------------|----------------------------|
//! | `Succeeded`         | `Passing`  | claim normally             |
//! | `Failed` / `Error`  | `Failing`  | circuit opens              |
//! | `Running`, missing  | `Unknown`  | claim normally (pending)   |
//! | no run at all       | `Unknown`  | claim normally             |
//!
//! A pending or missing verdict never opens the circuit: blocking on "no data
//! yet" would stop every worker the moment CI lagged a push, which is the
//! opposite failure to the one this module exists to fix. Only a completed,
//! failing run stops work.
//!
//! Results are cached per workflow template with a short TTL so that the
//! per-cycle selection loop does not hit the API on every pass.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::process::Command;

/// Cache TTL applied when no explicit TTL is given.
pub const DEFAULT_CACHE_TTL_SECS: u64 = 60;

/// Default read-only Argo Workflows endpoint (kubectl proxy on iad-ci).
pub const DEFAULT_CI_ENDPOINT: &str = "http://traefik-iad-ci:8001";

/// Environment variable overriding the Argo Workflows endpoint.
pub const CI_ENDPOINT_ENV: &str = "NEEDLE_CI_ENDPOINT";

/// Build status for a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildStatus {
    /// The newest CI run completed successfully.
    Passing,
    /// The newest CI run completed with a failure.
    Failing,
    /// No verdict is available (CI pending, absent, or unreachable).
    Unknown,
}

impl BuildStatus {
    /// Returns true if the build is failing.
    pub fn is_failing(self) -> bool {
        matches!(self, Self::Failing)
    }

    /// Returns true if the build is passing.
    pub fn is_passing(self) -> bool {
        matches!(self, Self::Passing)
    }

    /// Returns true if status is unknown.
    pub fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }
}

/// One failed step of a CI run, reduced to what names the failure.
///
/// Only Pod nodes are collected: the DAG and Steps nodes above them restate a
/// child's failure without adding information, so including them would turn
/// one broken step into four indistinguishable lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailedNode {
    /// Argo `displayName`, the step name an operator recognises
    /// (for example `verify-fast`).
    pub display_name: String,
    /// Argo's own message for the node (for example `main: Error (exit code 128)`).
    pub message: String,
}

/// One CI workflow run, reduced to what the circuit breaker decides on plus
/// what a reader needs to tell two different outages apart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CiWorkflowRun {
    /// Workflow name (for example `needle-ci-xg7fb`).
    pub name: String,
    /// Argo phase string, as reported by the API (`Succeeded`, `Failed`, …).
    pub phase: String,
    /// When the workflow was created, if the API reported a parseable stamp.
    pub created_at: Option<DateTime<Utc>>,
    /// The run's own `status.message`, when the API reported one.
    pub message: Option<String>,
    /// Failed Pod nodes, sorted by display name.
    ///
    /// Sorted on parse rather than on read: the API returns `status.nodes` as
    /// a JSON object, so iteration order is not stable, and an unsorted
    /// signature would differ between two reads of the same unchanged run —
    /// which would defeat both the determinism contract and any deduplication
    /// keyed on the signature.
    pub failed_nodes: Vec<FailedNode>,
}

impl CiWorkflowRun {
    /// Map an Argo phase onto a build status.
    ///
    /// Only a *completed* run yields `Passing` or `Failing`. Anything else —
    /// `Running`, `Pending`, a phase the API invented — is `Unknown`.
    pub fn status(&self) -> BuildStatus {
        match self.phase.as_str() {
            "Succeeded" => BuildStatus::Passing,
            "Failed" | "Error" => BuildStatus::Failing,
            _ => BuildStatus::Unknown,
        }
    }

    /// The failed-node signature: `<step>: <message>` per failed Pod node,
    /// joined by `; `.
    ///
    /// Falls back to the run's own `status.message` when no Pod node failed —
    /// a clone or an artifact step that dies before any pod starts leaves the
    /// node list empty, and "CI is red for no stated reason" is exactly the
    /// report that gets ignored.
    pub fn failed_node_signature(&self) -> String {
        if self.failed_nodes.is_empty() {
            return self.message.clone().unwrap_or_default();
        }
        self.failed_nodes
            .iter()
            .map(|node| format!("{}: {}", node.display_name, node.message))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Decides the build status for a named workflow template.
///
/// The trait exists so tests can supply a canned newest run instead of a live
/// cluster; production always uses [`ArgoWorkflowStatusSource`].
#[async_trait::async_trait]
pub trait CiStatusSource: Send + Sync {
    /// Returns the newest run of `template`, or `None` when the cluster has
    /// never run it.
    async fn newest_run(&self, template: &str) -> Result<Option<CiWorkflowRun>>;

    /// Every run of `template` the source can see, newest first.
    ///
    /// The circuit breaker only ever needs the newest run; a caller asking
    /// "how long has this been red?" needs the ones behind it. The default
    /// reports just the newest run, so a source written before this method
    /// existed stays correct — it simply cannot establish a streak longer
    /// than one, which reads as "not enough history" rather than as a clean
    /// bill of health.
    async fn run_history(&self, template: &str) -> Result<Vec<CiWorkflowRun>> {
        Ok(self.newest_run(template).await?.into_iter().collect())
    }
}

/// Read-only Argo Workflows API source.
pub struct ArgoWorkflowStatusSource {
    endpoint: String,
}

impl ArgoWorkflowStatusSource {
    /// Source pointed at the documented credential-free endpoint, or the
    /// `NEEDLE_CI_ENDPOINT` override.
    pub fn from_env() -> Self {
        let endpoint =
            std::env::var(CI_ENDPOINT_ENV).unwrap_or_else(|_| DEFAULT_CI_ENDPOINT.to_string());
        Self { endpoint }
    }

    /// Source pointed at an explicit endpoint.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }

    /// Reduce a workflow list to the newest run of `template`.
    ///
    /// Runs are matched on `spec.workflowTemplateRef.name`; a run that does
    /// not name a template (or names another one) is not this template's run.
    fn newest_run_of_template(list: &WorkflowList, template: &str) -> Option<CiWorkflowRun> {
        Self::runs_of_template(list, template).into_iter().next()
    }

    /// Every run of `template` in the list, newest first.
    ///
    /// Ordering: stamped runs descending by creation time, unstamped runs
    /// last in list order. The sort is stable, so this keeps
    /// [`Self::newest_run_of_template`]'s original rule — a stamped run
    /// always outranks an unstamped one, and among unstamped runs the first
    /// the API listed wins — while also giving a caller the runs behind it.
    fn runs_of_template(list: &WorkflowList, template: &str) -> Vec<CiWorkflowRun> {
        let mut runs: Vec<CiWorkflowRun> = list
            .items
            .iter()
            .filter(|item| {
                item.spec
                    .workflow_template_ref
                    .as_ref()
                    .and_then(|reference| reference.name.as_deref())
                    == Some(template)
            })
            .map(Self::run_from_workflow)
            .collect();
        // `None` sorts last: `Option`'s own ordering puts it first, which
        // would rank an unstamped run above every stamped one.
        runs.sort_by(|a, b| match (b.created_at, a.created_at) {
            (Some(newer), Some(older)) => newer.cmp(&older),
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (None, None) => std::cmp::Ordering::Equal,
        });
        runs
    }

    /// Reduce one Argo workflow object to the run facts this module exposes.
    fn run_from_workflow(item: &ArgoWorkflow) -> CiWorkflowRun {
        let status = item.status.as_ref();
        let mut failed_nodes: Vec<FailedNode> = status
            .map(|status| {
                status
                    .nodes
                    .values()
                    .filter(|node| {
                        node.node_type == "Pod"
                            && matches!(node.phase.as_deref(), Some("Failed") | Some("Error"))
                    })
                    .map(|node| FailedNode {
                        display_name: node.display_name.clone(),
                        message: node.message.clone().unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        failed_nodes
            .sort_by(|a, b| (&a.display_name, &a.message).cmp(&(&b.display_name, &b.message)));

        CiWorkflowRun {
            name: item.metadata.name.clone(),
            phase: status
                .and_then(|status| status.phase.clone())
                .unwrap_or_default(),
            created_at: item
                .metadata
                .creation_timestamp
                .as_deref()
                .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
                .map(|stamp| stamp.with_timezone(&Utc)),
            message: status.and_then(|status| status.message.clone()),
            failed_nodes,
        }
    }

    /// Fetch the namespace's whole workflow list once.
    ///
    /// Both trait methods read the same list: the API has no per-template
    /// query, so asking twice would double the cost without adding anything.
    async fn fetch_list(&self) -> Result<WorkflowList> {
        let url = format!(
            "{}/apis/argoproj.io/v1alpha1/namespaces/argo-workflows/workflows",
            self.endpoint.trim_end_matches('/')
        );
        let response = tokio::task::spawn_blocking(move || {
            ureq::get(&url)
                .timeout(Duration::from_secs(30))
                .call()
                .map_err(|error| anyhow!(error.to_string()))
                .and_then(|response| response.into_string().map_err(|e| anyhow!(e.to_string())))
        })
        .await
        .map_err(|error| anyhow!("CI workflow query task failed: {error}"))??;

        serde_json::from_str(&response).context("CI workflow list was not a valid WorkflowList")
    }
}

#[async_trait::async_trait]
impl CiStatusSource for ArgoWorkflowStatusSource {
    async fn newest_run(&self, template: &str) -> Result<Option<CiWorkflowRun>> {
        let list = self.fetch_list().await?;
        Ok(Self::newest_run_of_template(&list, template))
    }

    async fn run_history(&self, template: &str) -> Result<Vec<CiWorkflowRun>> {
        let list = self.fetch_list().await?;
        Ok(Self::runs_of_template(&list, template))
    }
}

/// Subset of the Argo `WorkflowList` shape the circuit breaker reads.
#[derive(Debug, Deserialize)]
struct WorkflowList {
    #[serde(default)]
    items: Vec<ArgoWorkflow>,
}

/// Subset of an Argo `Workflow` object.
#[derive(Debug, Clone, Deserialize)]
struct ArgoWorkflow {
    metadata: WorkflowMetadata,
    #[serde(default)]
    spec: WorkflowSpec,
    #[serde(default)]
    status: Option<WorkflowStatus>,
}

/// Subset of the workflow status: the phase the circuit breaker decides on,
/// plus the message and node set that name *why* a run failed.
#[derive(Debug, Clone, Default, Deserialize)]
struct WorkflowStatus {
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    message: Option<String>,
    /// Argo returns this as a JSON object keyed by node id, so its iteration
    /// order is not stable; everything read out of it is sorted before use.
    #[serde(default)]
    nodes: HashMap<String, ArgoNode>,
}

/// Subset of one Argo workflow node.
#[derive(Debug, Clone, Default, Deserialize)]
struct ArgoNode {
    #[serde(default, rename = "displayName")]
    display_name: String,
    #[serde(default, rename = "type")]
    node_type: String,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Subset of workflow object metadata.
#[derive(Debug, Clone, Deserialize)]
struct WorkflowMetadata {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "creationTimestamp")]
    creation_timestamp: Option<String>,
}

/// Subset of the workflow spec: only the template reference matters here.
#[derive(Debug, Clone, Deserialize, Default)]
struct WorkflowSpec {
    #[serde(default, rename = "workflowTemplateRef")]
    workflow_template_ref: Option<WorkflowTemplateRef>,
}

/// The `workflowTemplateRef` field of a workflow spec.
#[derive(Debug, Clone, Deserialize)]
struct WorkflowTemplateRef {
    #[serde(default)]
    name: Option<String>,
}

/// Cached build status with expiration time.
#[derive(Debug, Clone)]
struct CachedStatus {
    run: Option<CiWorkflowRun>,
    expires_at: DateTime<Utc>,
}

/// Checks and caches the build status of a workspace's CI workflow.
pub struct BuildStatusChecker {
    source: Arc<dyn CiStatusSource>,
    cache: Arc<RwLock<HashMap<String, CachedStatus>>>,
    cache_ttl_secs: u64,
}

impl BuildStatusChecker {
    /// Checker backed by the production Argo endpoint and the default TTL.
    pub fn production() -> Self {
        Self::with_source(
            DEFAULT_CACHE_TTL_SECS,
            Arc::new(ArgoWorkflowStatusSource::from_env()),
        )
    }

    /// Checker backed by an injected source (tests, operators).
    pub fn with_source(cache_ttl_secs: u64, source: Arc<dyn CiStatusSource>) -> Self {
        Self {
            source,
            cache: Arc::new(RwLock::new(HashMap::new())),
            cache_ttl_secs,
        }
    }

    /// Build status for a workspace, derived from its CI workflow template.
    ///
    /// Fails only when the workspace's CI template cannot be determined (no
    /// git remote, or a remote this convention cannot name a template for).
    /// A source failure is *not* an error: it degrades to
    /// [`BuildStatus::Unknown`], which never opens the circuit.
    pub async fn check_status(&self, workspace: &Path) -> Result<BuildStatus> {
        let template = template_for_workspace(workspace).await?;
        Ok(self.status_for_template(&template).await)
    }

    /// Build status for a named workflow template, cached for the TTL.
    ///
    /// Source failures and absent runs both yield `Unknown`: the circuit
    /// breaker opens only on evidence that a run failed, never on evidence
    /// being unavailable.
    pub async fn status_for_template(&self, template: &str) -> BuildStatus {
        self.newest_run_for_template(template)
            .await
            .map(|run| run.status())
            .unwrap_or(BuildStatus::Unknown)
    }

    /// The newest run of a workflow template, cached for the TTL.
    ///
    /// `None` covers "the template has never run" *and* "the source could not
    /// be asked" — a caller that needs the distinction does not have one,
    /// because neither state is evidence of a failure.
    pub async fn newest_run_for_template(&self, template: &str) -> Option<CiWorkflowRun> {
        if let Some(cached) = self.fresh_entry(template) {
            return cached;
        }

        let run = match self.source.newest_run(template).await {
            Ok(run) => run,
            Err(error) => {
                tracing::warn!(
                    template = %template,
                    error = %error,
                    "CI status source failed; treating build status as Unknown"
                );
                None
            }
        };
        if let Some(run) = &run {
            tracing::debug!(
                template = %template,
                workflow = %run.name,
                phase = %run.phase,
                "Newest CI run decides the build status"
            );
        } else {
            tracing::debug!(
                template = %template,
                "No CI run available for template; treating build status as Unknown"
            );
        }

        self.store(template, run.clone());
        run
    }

    /// Every run of a workflow template, newest first.
    ///
    /// Unlike [`BuildStatusChecker::newest_run_for_template`] this surfaces
    /// the source's error instead of folding it into an empty history. The
    /// circuit breaker is right to treat "nobody could look" as no evidence
    /// of a failure; a caller asking *how long* CI has been red is asking the
    /// opposite question, and an empty history would answer it with a clean
    /// bill of health the source never gave.
    ///
    /// Deliberately uncached: the history is read once per audit run, and
    /// sharing the breaker's TTL would let a cache decide whether an outage
    /// is visible.
    pub async fn run_history_for_template(&self, template: &str) -> Result<Vec<CiWorkflowRun>> {
        self.source.run_history(template).await
    }

    /// Drop every cached status (used by tests and by operators forcing a
    /// re-read).
    pub fn clear_cache(&self) {
        if let Ok(mut cache) = self.cache.write() {
            cache.clear();
        }
    }

    /// The cached verdict for `template` if one is fresh, where `None` inside
    /// the outer `Some` means "asked recently and there was no run".
    fn fresh_entry(&self, template: &str) -> Option<Option<CiWorkflowRun>> {
        let cache = self.cache.read().ok()?;
        let cached = cache.get(template)?;
        (cached.expires_at > Utc::now()).then_some(cached.run.clone())
    }

    fn store(&self, template: &str, run: Option<CiWorkflowRun>) {
        if let Ok(mut cache) = self.cache.write() {
            cache.insert(
                template.to_string(),
                CachedStatus {
                    run,
                    expires_at: Utc::now() + chrono::Duration::seconds(self.cache_ttl_secs as i64),
                },
            );
        }
    }
}

/// Name the CI workflow template that watches `workspace`'s remote.
///
/// The fleet convention is `<repo>-ci` on the repository's lowercase name —
/// `jedarden/NEEDLE` is watched by `needle-ci`. A template this convention
/// cannot name is an error, which surfaces as "no verdict" upstream rather
/// than as a wrong verdict.
pub async fn template_for_workspace(workspace: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(workspace)
        .output()
        .await
        .context("failed to read the workspace git remote")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git config remote.origin.url failed in {}",
            workspace.display()
        ));
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() {
        return Err(anyhow!(
            "workspace {} has no remote.origin.url",
            workspace.display()
        ));
    }
    let repo = url
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .strip_suffix(".git")
        .unwrap_or_else(|| url.rsplit('/').next().unwrap_or_default());
    if repo.is_empty() {
        return Err(anyhow!("could not name a repository from remote {url}"));
    }
    Ok(template_for_repo(repo))
}

/// Apply the fleet naming convention to a repository name.
pub fn template_for_repo(repo: &str) -> String {
    format!("{}-ci", repo.trim().trim_end_matches(".git").to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn run(name: &str, created: &str, phase: &str) -> ArgoWorkflow {
        ArgoWorkflow {
            metadata: WorkflowMetadata {
                name: name.to_string(),
                creation_timestamp: Some(created.to_string()),
            },
            spec: WorkflowSpec {
                workflow_template_ref: Some(WorkflowTemplateRef {
                    name: Some("needle-ci".to_string()),
                }),
            },
            status: Some(WorkflowStatus {
                phase: Some(phase.to_string()),
                ..WorkflowStatus::default()
            }),
        }
    }

    fn list(items: Vec<ArgoWorkflow>) -> WorkflowList {
        WorkflowList { items }
    }

    /// Source returning a fixed run (or none), recording the templates asked.
    struct FixedSource {
        run: Option<CiWorkflowRun>,
        error: Option<String>,
        asked: Mutex<Vec<String>>,
    }

    impl FixedSource {
        fn failing_run() -> Self {
            Self {
                run: Some(CiWorkflowRun {
                    name: "needle-ci-xg7fb".to_string(),
                    phase: "Failed".to_string(),
                    created_at: Some(Utc::now()),
                    ..CiWorkflowRun::default()
                }),
                error: None,
                asked: Mutex::new(Vec::new()),
            }
        }

        fn none() -> Self {
            Self {
                run: None,
                error: None,
                asked: Mutex::new(Vec::new()),
            }
        }

        fn error(message: &str) -> Self {
            Self {
                run: None,
                error: Some(message.to_string()),
                asked: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl CiStatusSource for FixedSource {
        async fn newest_run(&self, template: &str) -> Result<Option<CiWorkflowRun>> {
            self.asked.lock().unwrap().push(template.to_string());
            if let Some(error) = &self.error {
                return Err(anyhow!("{}", error));
            }
            Ok(self.run.clone())
        }
    }

    #[test]
    fn build_status_predicates_agree() {
        assert!(BuildStatus::Failing.is_failing());
        assert!(!BuildStatus::Passing.is_failing());
        assert!(!BuildStatus::Unknown.is_failing());
        assert!(BuildStatus::Passing.is_passing());
        assert!(BuildStatus::Unknown.is_unknown());
        assert!(!BuildStatus::Failing.is_unknown());
    }

    #[test]
    fn completed_phases_decide_the_verdict() {
        let verdict = |phase: &str| {
            CiWorkflowRun {
                name: "needle-ci-abcde".to_string(),
                phase: phase.to_string(),
                created_at: None,
                ..CiWorkflowRun::default()
            }
            .status()
        };

        assert_eq!(verdict("Succeeded"), BuildStatus::Passing);
        assert_eq!(verdict("Failed"), BuildStatus::Failing);
        assert_eq!(verdict("Error"), BuildStatus::Failing);
        // A pending or unknown phase is never evidence of a failure.
        assert_eq!(verdict("Running"), BuildStatus::Unknown);
        assert_eq!(verdict("Pending"), BuildStatus::Unknown);
        assert_eq!(verdict(""), BuildStatus::Unknown);
    }

    #[test]
    fn newest_run_of_template_selects_latest_and_ignores_other_templates() {
        let list = list(vec![
            run("needle-ci-old", "2026-09-05T13:11:40Z", "Failed"),
            run("needle-ci-new", "2026-09-05T15:34:59Z", "Succeeded"),
            // Another template entirely: a supersede action, not a build.
            ArgoWorkflow {
                metadata: WorkflowMetadata {
                    name: "needle-ci-supersede-jj6c6".to_string(),
                    creation_timestamp: Some("2026-09-05T16:00:00Z".to_string()),
                },
                spec: WorkflowSpec {
                    workflow_template_ref: Some(WorkflowTemplateRef {
                        name: Some("needle-ci-supersede".to_string()),
                    }),
                },
                status: Some(WorkflowStatus {
                    phase: Some("Failed".to_string()),
                    ..WorkflowStatus::default()
                }),
            },
        ]);

        let newest = ArgoWorkflowStatusSource::newest_run_of_template(&list, "needle-ci");
        assert_eq!(newest.expect("a run exists").name, "needle-ci-new");
    }

    #[test]
    fn newest_run_of_template_is_empty_without_matches() {
        let named = list(vec![run("needle-ci-old", "2026-09-05T13:11:40Z", "Failed")]);
        assert!(
            ArgoWorkflowStatusSource::newest_run_of_template(&named, "other-repo-ci").is_none()
        );
        assert!(
            ArgoWorkflowStatusSource::newest_run_of_template(&list(vec![]), "needle-ci").is_none()
        );
    }

    #[test]
    fn newest_run_of_template_survives_an_unstamped_run() {
        let mut unstamped = run("needle-ci-unstamped", "2026-09-05T14:00:00Z", "Failed");
        unstamped.metadata.creation_timestamp = None;

        let with_stamped = list(vec![
            unstamped.clone(),
            run("needle-ci-new", "2026-09-05T15:34:59Z", "Succeeded"),
        ]);
        let newest = ArgoWorkflowStatusSource::newest_run_of_template(&with_stamped, "needle-ci")
            .expect("a run exists");
        // The stamped run wins; an unstamped one never outranks a stamped one.
        assert_eq!(newest.name, "needle-ci-new");

        let only_unstamped = list(vec![unstamped]);
        assert_eq!(
            ArgoWorkflowStatusSource::newest_run_of_template(&only_unstamped, "needle-ci")
                .expect("the unstamped run is still returned")
                .name,
            "needle-ci-unstamped"
        );
    }

    #[test]
    fn workflow_list_parses_the_api_shape() {
        let payload = r#"{
            "metadata": {"resourceVersion": "75315431"},
            "items": [{
                "metadata": {"name": "needle-ci-skr7d", "creationTimestamp": "2026-09-05T15:34:59Z"},
                "spec": {"workflowTemplateRef": {"name": "needle-ci"}},
                "status": {"phase": "Running"}
            }]
        }"#;
        let list: WorkflowList = serde_json::from_str(payload).expect("parses");
        let newest = ArgoWorkflowStatusSource::newest_run_of_template(&list, "needle-ci")
            .expect("the running run is listed");
        assert_eq!(newest.name, "needle-ci-skr7d");
        assert_eq!(newest.phase, "Running");
        assert_eq!(newest.status(), BuildStatus::Unknown);
    }

    #[tokio::test]
    async fn failing_run_opens_the_circuit() {
        let checker = BuildStatusChecker::with_source(60, Arc::new(FixedSource::failing_run()));
        assert_eq!(
            checker.status_for_template("needle-ci").await,
            BuildStatus::Failing
        );
    }

    #[tokio::test]
    async fn absent_runs_and_source_errors_stay_unknown() {
        let absent = BuildStatusChecker::with_source(60, Arc::new(FixedSource::none()));
        assert_eq!(
            absent.status_for_template("needle-ci").await,
            BuildStatus::Unknown
        );

        let broken =
            BuildStatusChecker::with_source(60, Arc::new(FixedSource::error("connection refused")));
        assert_eq!(
            broken.status_for_template("needle-ci").await,
            BuildStatus::Unknown
        );
    }

    #[tokio::test]
    async fn status_is_cached_within_the_ttl() {
        let source = Arc::new(FixedSource::failing_run());
        let checker = BuildStatusChecker::with_source(600, source.clone());

        assert_eq!(
            checker.status_for_template("needle-ci").await,
            BuildStatus::Failing
        );
        assert_eq!(
            checker.status_for_template("needle-ci").await,
            BuildStatus::Failing
        );
        assert_eq!(
            source.asked.lock().unwrap().len(),
            1,
            "second read is cached"
        );

        checker.clear_cache();
        let _ = checker.status_for_template("needle-ci").await;
        assert_eq!(
            source.asked.lock().unwrap().len(),
            2,
            "clear_cache forces a re-read"
        );
    }

    /// A source that only implements `newest_run`, to exercise the trait's
    /// default `run_history`.
    struct NewestOnlySource(Option<CiWorkflowRun>);

    #[async_trait::async_trait]
    impl CiStatusSource for NewestOnlySource {
        async fn newest_run(&self, _template: &str) -> Result<Option<CiWorkflowRun>> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn nt57_run_history_parses_the_message_and_failed_pod_nodes() {
        // The shape the read-only proxy actually returns for a red run: a
        // status message, one failed Pod node, one failed DAG node restating
        // it, and one Pod that succeeded.
        let payload = r#"{
            "items": [{
                "metadata": {"name": "needle-ci-xg7fb", "creationTimestamp": "2026-09-14T22:16:00Z"},
                "spec": {"workflowTemplateRef": {"name": "needle-ci"}},
                "status": {
                    "phase": "Failed",
                    "message": "child 'needle-ci-xg7fb-1' failed",
                    "nodes": {
                        "needle-ci-xg7fb-1": {
                            "displayName": "verify-fast",
                            "type": "Pod",
                            "phase": "Error",
                            "message": "main: Error (exit code 128)"
                        },
                        "needle-ci-xg7fb": {
                            "displayName": "needle-ci-xg7fb",
                            "type": "DAG",
                            "phase": "Failed",
                            "message": "child failed"
                        },
                        "needle-ci-xg7fb-2": {
                            "displayName": "clone",
                            "type": "Pod",
                            "phase": "Succeeded"
                        }
                    }
                }
            }]
        }"#;
        let list: WorkflowList = serde_json::from_str(payload).expect("parses");
        let runs = ArgoWorkflowStatusSource::runs_of_template(&list, "needle-ci");

        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert_eq!(run.status(), BuildStatus::Failing);
        assert_eq!(
            run.message.as_deref(),
            Some("child 'needle-ci-xg7fb-1' failed")
        );
        // Only the Pod node: the DAG above it restates the same failure, and
        // the Pod that succeeded is not a failure at all.
        assert_eq!(
            run.failed_nodes,
            vec![FailedNode {
                display_name: "verify-fast".to_string(),
                message: "main: Error (exit code 128)".to_string(),
            }]
        );
        assert_eq!(
            run.failed_node_signature(),
            "verify-fast: main: Error (exit code 128)"
        );

        // A run with no failed Pod node still names a reason rather than
        // reporting an empty signature.
        let no_nodes = CiWorkflowRun {
            phase: "Failed".to_string(),
            message: Some("clone returned HTTP 503".to_string()),
            ..CiWorkflowRun::default()
        };
        assert_eq!(no_nodes.failed_node_signature(), "clone returned HTTP 503");
    }

    #[tokio::test]
    async fn nt57_run_history_is_newest_first_and_defaults_to_the_newest_run() {
        let mut unstamped = run("needle-ci-unstamped", "2026-09-14T00:00:00Z", "Failed");
        unstamped.metadata.creation_timestamp = None;
        let list = list(vec![
            run("needle-ci-old", "2026-09-14T22:16:00Z", "Failed"),
            unstamped,
            run("needle-ci-new", "2026-09-15T18:00:00Z", "Failed"),
            run("needle-ci-mid", "2026-09-15T02:00:00Z", "Succeeded"),
        ]);

        let ordered = ArgoWorkflowStatusSource::runs_of_template(&list, "needle-ci");
        let names: Vec<&str> = ordered.iter().map(|run| run.name.as_str()).collect();
        // Stamped runs newest first; the unstamped run sorts last rather than
        // outranking every stamped one, which is what `Option`'s own ordering
        // would have done.
        assert_eq!(
            names,
            vec![
                "needle-ci-new",
                "needle-ci-mid",
                "needle-ci-old",
                "needle-ci-unstamped"
            ]
        );
        // The newest run is still the head of the history.
        assert_eq!(
            ArgoWorkflowStatusSource::newest_run_of_template(&list, "needle-ci")
                .expect("a run exists")
                .name,
            "needle-ci-new"
        );

        // A source predating `run_history` reports the newest run alone —
        // never an empty history, which would read as "nothing ever failed".
        let newest = CiWorkflowRun {
            name: "needle-ci-only".to_string(),
            phase: "Failed".to_string(),
            ..CiWorkflowRun::default()
        };
        let source = NewestOnlySource(Some(newest.clone()));
        assert_eq!(source.run_history("needle-ci").await.unwrap(), vec![newest]);
        assert!(NewestOnlySource(None)
            .run_history("needle-ci")
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn template_names_follow_the_fleet_convention() {
        assert_eq!(template_for_repo("NEEDLE"), "needle-ci");
        assert_eq!(template_for_repo("AgentScribe"), "agentscribe-ci");
        assert_eq!(template_for_repo("needle.git"), "needle-ci");
    }
}
