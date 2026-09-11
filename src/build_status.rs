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

/// One CI workflow run, reduced to what the circuit breaker decides on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiWorkflowRun {
    /// Workflow name (for example `needle-ci-xg7fb`).
    pub name: String,
    /// Argo phase string, as reported by the API (`Succeeded`, `Failed`, …).
    pub phase: String,
    /// When the workflow was created, if the API reported a parseable stamp.
    pub created_at: Option<DateTime<Utc>>,
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
        let mut newest: Option<(&ArgoWorkflow, Option<DateTime<Utc>>)> = None;
        for item in &list.items {
            let named = item
                .spec
                .workflow_template_ref
                .as_ref()
                .and_then(|reference| reference.name.as_deref());
            if named != Some(template) {
                continue;
            }
            let created_at = item
                .metadata
                .creation_timestamp
                .as_deref()
                .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
                .map(|stamp| stamp.with_timezone(&Utc));
            let replace = match (&newest, created_at) {
                // A stamped run beats anything seen so far that is older — or
                // that has no stamp at all.
                (Some((_, Some(seen))), Some(created)) => created > *seen,
                (Some((_, None)), Some(_)) => true,
                (Some(_), None) => false,
                (None, _) => true,
            };
            if replace {
                newest = Some((item, created_at));
            }
        }
        newest.map(|(item, created_at)| CiWorkflowRun {
            name: item.metadata.name.clone(),
            phase: item
                .status
                .as_ref()
                .and_then(|status| status.phase.clone())
                .unwrap_or_default(),
            created_at,
        })
    }
}

#[async_trait::async_trait]
impl CiStatusSource for ArgoWorkflowStatusSource {
    async fn newest_run(&self, template: &str) -> Result<Option<CiWorkflowRun>> {
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

        let list: WorkflowList = serde_json::from_str(&response)
            .context("CI workflow list was not a valid WorkflowList")?;
        Ok(Self::newest_run_of_template(&list, template))
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

/// Subset of the workflow status: only the phase matters here.
#[derive(Debug, Clone, Deserialize)]
struct WorkflowStatus {
    #[serde(default)]
    phase: Option<String>,
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

    #[test]
    fn template_names_follow_the_fleet_convention() {
        assert_eq!(template_for_repo("NEEDLE"), "needle-ci");
        assert_eq!(template_for_repo("AgentScribe"), "agentscribe-ci");
        assert_eq!(template_for_repo("needle.git"), "needle-ci");
    }

    #[tokio::test]
    async fn workspace_template_resolution_names_the_repo_template() {
        // This repository is the canonical example: its remote names NEEDLE.
        let template = template_for_workspace(Path::new(env!("CARGO_MANIFEST_DIR")))
            .await
            .expect("the manifest dir is a git checkout with a remote");
        assert_eq!(template, "needle-ci");
    }
}
