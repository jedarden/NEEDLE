//! Experimental idle-time gate reattempt for gate-degraded workspaces.
//!
//! Gate degradation is a starvation loop by design: Pluck and Explore skip a
//! degraded workspace for ordinary dispatch (N-T22), and the flag clears only
//! on a *successful gate run in that workspace* — which the skip makes
//! impossible. A workspace whose gate recovered (or whose fingerprint
//! degradation described beads that are long gone) stays blocked until a human
//! pins a worker at it, and the 2026-10-23 census found 17 workspaces in
//! exactly that state, most with stale or zero-error degradation records.
//!
//! The reattempt breaks the loop from the idle side: when a worker's strand
//! waterfall finds no work at all, it probes one due degraded workspace by
//! running that workspace's own resolved verification gates against the
//! current committed state — without claiming a bead. A passing probe runs the
//! existing restore path (`OutcomeHandler::restore_degraded_workspace`), so
//! clearing looks exactly like a real recovery: state file removed, "Gate
//! broken" alert beads closed, `workspace.gate_restored` emitted. A failing or
//! inconclusive probe doubles the wait before the next probe, capped.
//!
//! The probe must never compete with real work, so it runs only when the
//! waterfall returned no candidate and not even found-but-excluded work — the
//! worker would otherwise be sleeping. Probe verdicts are per-workspace state
//! under `state/gate-reattempt/`, shared by every worker on the host through
//! the same file the backoff is persisted to, so two idle workers cannot
//! double-probe the same workspace inside one backoff window.

use crate::config::DefaultGatesConfig;
use crate::types::{Bead, BeadId, BeadStatus};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The synthetic bead id a probe presents to gate execution.
///
/// Gates receive `NEEDLE_BEAD_ID`/`NEEDLE_WORKSPACE` and clean extractions
/// name themselves after the bead; the probe has no bead, so it names itself.
/// The id is never created in any bead store — it exists only as a parameter
/// carrier and as the `bead_id` marker on the `workspace.gate_restored` event.
pub const PROBE_BEAD_ID: &str = "gate-reattempt-probe";

/// Provenance string passed to the restore path when a probe passes.
pub const PROBE_PROVENANCE: &str =
    "the idle gate reattempt probe — the workspace's own gates ran and passed at current HEAD";

/// Configuration for the experimental idle gate reattempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateReattemptConfig {
    /// Whether the idle probe runs at all. Default true: the probe only ever
    /// fires when the worker has nothing else to do, and degradation is
    /// already the conservative state — a wrong restore re-degrades itself on
    /// the next real failure.
    pub enabled: bool,
    /// Wait before the second probe after a failure; every further failure
    /// doubles it, capped at [`GateReattemptConfig::max_backoff_secs`].
    pub initial_backoff_secs: u64,
    /// Upper bound on the probe backoff.
    pub max_backoff_secs: u64,
}

impl Default for GateReattemptConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            initial_backoff_secs: 1800,
            max_backoff_secs: 86_400,
        }
    }
}

/// Persisted probe schedule for one degraded workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateReattemptState {
    /// Workspace path (canonicalized, matching the gate-health record).
    pub workspace: PathBuf,
    /// Backoff applied after the most recent failed probe.
    pub backoff_secs: u64,
    /// Earliest instant the next probe may run (RFC 3339).
    pub next_probe_at: String,
    /// How many probes have failed since the workspace was first probed.
    pub consecutive_probe_failures: u32,
    /// Human-readable reason the last probe did not clear, for the log.
    pub last_failure_reason: String,
}

impl GateReattemptState {
    /// First observation of a workspace: probe as soon as the worker is idle.
    fn first(workspace: PathBuf) -> Self {
        Self {
            workspace,
            backoff_secs: 0,
            next_probe_at: Utc::now().to_rfc3339(),
            consecutive_probe_failures: 0,
            last_failure_reason: String::new(),
        }
    }

    /// Record a non-clearing probe: schedule the next one one wait further
    /// out, doubling from [`GateReattemptConfig::initial_backoff_secs`] and
    /// capping at [`GateReattemptConfig::max_backoff_secs`].
    fn record_failure(&mut self, config: &GateReattemptConfig, reason: String) {
        self.backoff_secs = if self.backoff_secs == 0 {
            config.initial_backoff_secs
        } else {
            self.backoff_secs
                .saturating_mul(2)
                .min(config.max_backoff_secs)
        };
        self.next_probe_at =
            (Utc::now() + chrono::Duration::seconds(self.backoff_secs as i64)).to_rfc3339();
        self.consecutive_probe_failures += 1;
        self.last_failure_reason = reason;
    }

    /// Whether the probe may run at `now`.
    fn due_at(&self, now: DateTime<Utc>) -> bool {
        DateTime::parse_from_rfc3339(&self.next_probe_at)
            .map(|next| next <= now)
            .unwrap_or(true)
    }
}

/// The probe state directory: `<root>/state/gate-reattempt`.
fn state_file_path(workspace: &Path) -> Result<PathBuf> {
    let id = crate::gate_health::workspace_id(workspace)?;
    Ok(crate::state_dir::gate_reattempt_dir().join(format!("{id}.json")))
}

/// Load the persisted probe schedule for a workspace, if any.
pub fn load_state(workspace: &Path) -> Result<Option<GateReattemptState>> {
    let path = state_file_path(workspace)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let state: GateReattemptState = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(state))
}

/// Persist the probe schedule for a workspace.
pub fn save_state(state: &GateReattemptState) -> Result<()> {
    let path = state_file_path(&state.workspace)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("gate reattempt state path has no parent"))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    let content =
        serde_json::to_string_pretty(state).context("failed to serialize gate reattempt state")?;
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Remove the probe schedule after a successful restore.
pub fn clear_state(workspace: &Path) -> Result<()> {
    let path = state_file_path(workspace)?;
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

/// Degraded workspaces whose probe is due, most starved first.
///
/// The degraded set is enumerated from the gate-health records — the same
/// state Pluck and Explore skip on — not from the probe-schedule directory:
/// a workspace that has never been probed has no schedule file yet, and
/// keying the scan on schedules would leave exactly the starved backlog
/// this probe exists to drain permanently undiscoverable. Each degraded
/// workspace is then joined against its schedule: never-probed workspaces
/// are due immediately (the first idle encounter is the first probe) and
/// sort by how long their gate has been broken; probed workspaces are due
/// when their schedule says so and sort by how overdue they are. A
/// schedule whose workspace is no longer degraded — a restore that raced
/// this scan — is stale and dropped.
pub fn due_workspaces(now: DateTime<Utc>) -> Result<Vec<PathBuf>> {
    let health_dir = crate::state_dir::gate_health_dir();
    if !health_dir.exists() {
        return Ok(Vec::new());
    }

    // (sort key, workspace): never-probed workspaces sort by the age of
    // their degradation (oldest error first); probed ones sort after them
    // by scheduled probe time (most overdue first).
    let mut never_probed: Vec<(DateTime<Utc>, PathBuf)> = Vec::new();
    let mut scheduled: Vec<(DateTime<Utc>, PathBuf)> = Vec::new();

    for entry in std::fs::read_dir(&health_dir)
        .with_context(|| format!("failed to read {}", health_dir.display()))?
    {
        let path = entry
            .with_context(|| format!("failed to read {}", health_dir.display()))?
            .path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(content) = std::fs::read(&path) else {
            continue;
        };
        let state = match serde_json::from_slice::<crate::gate_health::GateHealthState>(&content) {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(path = %path.display(), error = %error, "ignoring unparseable gate health state");
                continue;
            }
        };
        if !state.degraded || !matches!(crate::gate_health::is_degraded(&state.workspace), Ok(true))
        {
            continue;
        }
        match load_state(&state.workspace) {
            Ok(Some(schedule)) => {
                if schedule.due_at(now) {
                    let at = schedule
                        .next_probe_at
                        .parse::<DateTime<Utc>>()
                        .unwrap_or(now);
                    scheduled.push((at, state.workspace));
                }
            }
            Ok(None) => {
                let last_error = state.last_error_at.parse::<DateTime<Utc>>().unwrap_or(now);
                never_probed.push((last_error, state.workspace));
            }
            Err(error) => {
                tracing::warn!(
                    workspace = %state.workspace.display(),
                    error = %error,
                    "gate reattempt: unreadable probe schedule — treating workspace as never probed"
                );
                never_probed.push((now, state.workspace));
            }
        }
    }

    // Schedules outliving their degradation are stale; drop them so the
    // schedule directory does not accumulate restored workspaces forever.
    if let Ok(entries) = std::fs::read_dir(crate::state_dir::gate_reattempt_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(state) = load_state_by_path(&path) else {
                continue;
            };
            if !matches!(crate::gate_health::is_degraded(&state.workspace), Ok(true)) {
                let _ = clear_state(&state.workspace);
            }
        }
    }

    never_probed.sort();
    scheduled.sort();
    let mut due: Vec<PathBuf> = never_probed.into_iter().map(|(_, ws)| ws).collect();
    due.extend(scheduled.into_iter().map(|(_, ws)| ws));
    Ok(due)
}

/// Parse a [`GateReattemptState`] straight from a schedule file path.
fn load_state_by_path(path: &Path) -> Option<GateReattemptState> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// What one probe decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeVerdict {
    /// Every resolved gate ran and passed — run the restore path.
    Passed {
        /// How many gates produced the verdict.
        gates_run: usize,
    },
    /// The gates ran and at least one rejected current HEAD — still degraded.
    Failed {
        /// Human-readable summary of the first rejection.
        reason: String,
    },
    /// The probe could not produce a verdict (no gates resolve, execution
    /// error, store/config failure) — treat exactly like a failure for
    /// scheduling, but never as evidence the workspace recovered.
    Inconclusive {
        /// Why no verdict was produced.
        reason: String,
    },
}

/// Run the degraded workspace's own verification gates at current HEAD,
/// without a bead.
///
/// Gates resolve from the workspace's live `.needle.yaml` — the same
/// `gates:`/`verification:` resolution ordinary dispatch uses — falling back
/// to the language default for a workspace that declares neither. A workspace
/// whose own config opts out of verification entirely (`gates: []`, or the
/// host-level fallback gate disabled with nothing detected) resolves no gate
/// to re-run, so the probe is [`ProbeVerdict::Inconclusive`]: only a real
/// gate run may clear a real degradation.
///
/// The workspace itself is the unit of judgement: gates run against current
/// HEAD (`RunIn::Clean` gates in a fresh extraction), with the synthetic
/// [`PROBE_BEAD_ID`] naming the run.
pub async fn run_gate_probe(
    workspace: &Path,
    default_gates: &DefaultGatesConfig,
    timeout: Duration,
) -> ProbeVerdict {
    let gates_config = match crate::config::gates_for_workspace(workspace) {
        Ok(config) => config,
        Err(error) => {
            return ProbeVerdict::Inconclusive {
                reason: format!("failed to resolve workspace gates: {error:#}"),
            }
        }
    };

    let mut named_gates: Vec<(String, crate::validation::GateConfig)> = gates_config
        .gates
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, gate)| (format!("gate_{index}"), gate))
        .collect();
    if !gates_config.verification.is_empty() {
        named_gates.push((
            "verification".to_string(),
            crate::validation::GateConfig::Command {
                commands: gates_config.verification.clone(),
                stderr_cap_bytes: None,
                run_in: crate::validation::RunIn::Workspace,
            },
        ));
    }
    if named_gates.is_empty() && !gates_config.declared {
        if let Some(detected) = crate::validation::default_gates::detect(workspace, default_gates) {
            tracing::info!(
                workspace = %workspace.display(),
                language = detected.language,
                "gate reattempt: workspace declares no gates — probing the language default"
            );
            named_gates.push((
                detected.gate_name(),
                crate::validation::GateConfig::Command {
                    commands: detected.commands,
                    stderr_cap_bytes: None,
                    run_in: crate::validation::RunIn::Clean,
                },
            ));
        }
    }

    if named_gates.is_empty() {
        return ProbeVerdict::Inconclusive {
            reason: "workspace resolves no gate to re-run (explicit opt-out or nothing detected)"
                .to_string(),
        };
    }

    let Some(validation_gate) =
        crate::validation::ValidationGate::new(named_gates, workspace.to_path_buf())
    else {
        return ProbeVerdict::Inconclusive {
            reason: "no gate could be constructed from the workspace config".to_string(),
        };
    };

    let probe_bead = Bead {
        id: BeadId::from(PROBE_BEAD_ID),
        title: "Gate reattempt probe (synthetic — not a store bead)".to_string(),
        body: None,
        priority: 4,
        status: BeadStatus::Open,
        assignee: None,
        labels: vec![],
        workspace: workspace.to_path_buf(),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let report = match tokio::time::timeout(timeout, validation_gate.run(&probe_bead)).await {
        Ok(Ok(report)) => report,
        Ok(Err(error)) => {
            return ProbeVerdict::Inconclusive {
                reason: format!("gate execution failed: {error:#}"),
            }
        }
        Err(_) => {
            return ProbeVerdict::Inconclusive {
                reason: format!("gate run timed out after {:?}", timeout),
            }
        }
    };

    if report.all_passed {
        return ProbeVerdict::Passed {
            gates_run: report.results.len(),
        };
    }

    // An execution error is the gate still being broken — the exact shape
    // that degraded the workspace in the first place. It must not clear.
    for (name, result) in &report.results {
        if let crate::validation::GateResult::ExecutionError { command, reason } = result {
            return ProbeVerdict::Failed {
                reason: format!("gate '{name}' still cannot execute ({command}): {reason}"),
            };
        }
    }
    let reason = report
        .results
        .values()
        .find_map(|result| result.failure_reason().map(str::to_string))
        .unwrap_or_else(|| "verification failed".to_string());
    ProbeVerdict::Failed { reason }
}

/// Whether an idle worker may probe at all.
///
/// "Must never compete with real work": the waterfall must have found no
/// candidate *and* no found-but-excluded work. Found-but-excluded means the
/// queue holds work this worker cannot take — the fleet is not idle, and the
/// probe waits.
pub fn probe_allowed_on_idle(enabled: bool, found_but_excluded: bool) -> bool {
    enabled && !found_but_excluded
}

/// What one orchestration pass did, for the worker's log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdleProbeOutcome {
    /// Nothing was due.
    Idle,
    /// A probe ran and produced this verdict.
    Probed {
        workspace: PathBuf,
        verdict: ProbeVerdict,
    },
}

/// Probe the single most overdue degraded workspace and act on the verdict.
///
/// `restore` is the existing restoration path, parameterized by workspace and
/// by how many gates proved the recovery, so the caller binds it to a
/// workspace-scoped store; tests inject a recorder. Exactly one workspace is
/// probed per call — the most overdue one — so a fleet of idle workers drains
/// the degraded set one gate run at a time without any worker spending its
/// whole idle window running gates.
pub async fn probe_most_overdue<F, Fut>(
    config: &GateReattemptConfig,
    default_gates: &DefaultGatesConfig,
    timeout: Duration,
    restore: F,
) -> Result<IdleProbeOutcome>
where
    F: FnOnce(PathBuf, usize) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let now = Utc::now();
    let Some(workspace) = due_workspaces(now)?.into_iter().next() else {
        return Ok(IdleProbeOutcome::Idle);
    };

    tracing::info!(
        workspace = %workspace.display(),
        "gate reattempt: worker idle — probing degraded workspace's gates"
    );

    let verdict = run_gate_probe(&workspace, default_gates, timeout).await;
    match &verdict {
        ProbeVerdict::Passed { gates_run } => {
            match restore(workspace.clone(), *gates_run).await {
                Ok(()) => {
                    clear_state(&workspace)?;
                    tracing::info!(
                        workspace = %workspace.display(),
                        gates_run,
                        "gate reattempt: probe passed — workspace restored"
                    );
                }
                Err(error) => {
                    // The gate run proved recovery but the restore path
                    // failed (store open, alert close). Keep the probe
                    // schedule so another idle cycle retries the restore;
                    // do not treat this as a gate failure.
                    tracing::error!(
                        workspace = %workspace.display(),
                        error = %error,
                        "gate reattempt: probe passed but restoration failed — will re-probe"
                    );
                    let mut state = load_state(&workspace)?
                        .unwrap_or_else(|| GateReattemptState::first(workspace.clone()));
                    state.record_failure(
                        config,
                        format!("restore failed after passing probe: {error:#}"),
                    );
                    save_state(&state)?;
                }
            }
        }
        ProbeVerdict::Failed { reason } | ProbeVerdict::Inconclusive { reason } => {
            let mut state = load_state(&workspace)?
                .unwrap_or_else(|| GateReattemptState::first(workspace.clone()));
            state.record_failure(config, reason.clone());
            save_state(&state)?;
            tracing::warn!(
                workspace = %workspace.display(),
                failures = state.consecutive_probe_failures,
                next_probe_at = %state.next_probe_at,
                backoff_secs = state.backoff_secs,
                reason = %reason,
                "gate reattempt: probe did not clear the workspace — backing off"
            );
        }
    }

    Ok(IdleProbeOutcome::Probed { workspace, verdict })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Isolated HOME so the gate-health and probe-schedule state roots land
    /// beneath a temporary directory instead of the operator's live
    /// `~/.needle` (gate-health refuses to record temporary workspaces into
    /// a durable HOME, and the real fleet must never see fixture state).
    fn isolated_home() -> (crate::util::test_env::EnvGuard, TempDir) {
        let guard = crate::util::test_env::isolate_env();
        let home = TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());
        (guard, home)
    }

    /// A workspace whose declared gate runs `command` in the checkout.
    fn gate_workspace(command: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join(".needle.yaml"),
            format!(
                "gates:\n  - type: command\n    commands:\n      - \"{command}\"\n    run_in: workspace\n"
            ),
        )
        .unwrap();
        dir
    }

    /// Degrade a workspace the way real repeated gate execution errors do.
    fn degrade(workspace: &Path) {
        for _ in 0..8 {
            let (_, degraded) = crate::gate_health::record_error(
                workspace,
                "gate-cmd".to_string(),
                "boom".to_string(),
            )
            .unwrap();
            if degraded {
                return;
            }
        }
        panic!("workspace never reached degradation threshold");
    }

    fn short_config() -> GateReattemptConfig {
        GateReattemptConfig {
            enabled: true,
            initial_backoff_secs: 5,
            max_backoff_secs: 20,
        }
    }

    fn schedule(workspace: &Path, next_probe_at: DateTime<Utc>) -> GateReattemptState {
        GateReattemptState {
            workspace: workspace.to_path_buf(),
            backoff_secs: 5,
            next_probe_at: next_probe_at.to_rfc3339(),
            consecutive_probe_failures: 1,
            last_failure_reason: "prior probe".to_string(),
        }
    }

    #[test]
    fn probe_only_when_idle_and_enabled() {
        // Idle and enabled: the probe runs.
        assert!(probe_allowed_on_idle(true, false));
        // Found-but-excluded work means the fleet is not idle.
        assert!(!probe_allowed_on_idle(true, true));
        // The experiment off disables the probe outright.
        assert!(!probe_allowed_on_idle(false, false));
    }

    #[test]
    fn failure_backoff_doubles_then_caps() {
        let config = short_config();
        let dir = TempDir::new().unwrap();
        let mut state = GateReattemptState::first(dir.path().to_path_buf());

        state.record_failure(&config, "one".to_string());
        assert_eq!(state.backoff_secs, 5);
        assert_eq!(state.consecutive_probe_failures, 1);

        state.record_failure(&config, "two".to_string());
        assert_eq!(state.backoff_secs, 10);

        state.record_failure(&config, "three".to_string());
        assert_eq!(state.backoff_secs, 20);

        // Further failures hold at the cap instead of growing without bound.
        state.record_failure(&config, "four".to_string());
        assert_eq!(state.backoff_secs, 20);
        assert_eq!(state.consecutive_probe_failures, 4);
        assert_eq!(state.last_failure_reason, "four");

        let next = DateTime::parse_from_rfc3339(&state.next_probe_at).unwrap();
        assert!(next > Utc::now());
    }

    #[tokio::test]
    async fn due_lists_never_probed_first_and_drops_stale_schedules() {
        let (_guard, _home) = isolated_home();
        let now = Utc::now();

        let never = TempDir::new().unwrap();
        degrade(never.path());

        let waiting = TempDir::new().unwrap();
        degrade(waiting.path());
        save_state(&schedule(waiting.path(), now + chrono::Duration::hours(1))).unwrap();

        let due_now = TempDir::new().unwrap();
        degrade(due_now.path());
        save_state(&schedule(
            due_now.path(),
            now - chrono::Duration::minutes(5),
        ))
        .unwrap();

        // Restored since its last probe: the schedule is stale and goes away.
        let restored = TempDir::new().unwrap();
        save_state(&schedule(
            restored.path(),
            now - chrono::Duration::minutes(5),
        ))
        .unwrap();

        let due = due_workspaces(now).unwrap();
        assert_eq!(
            due,
            vec![never.path().to_path_buf(), due_now.path().to_path_buf()],
            "never-probed workspaces drain before scheduled retries"
        );
        assert!(
            load_state(restored.path()).unwrap().is_none(),
            "stale schedule for a restored workspace must be dropped"
        );
    }

    #[tokio::test]
    async fn passing_probe_restores_clears_and_reports_gates() {
        let (_guard, _home) = isolated_home();
        let workspace = gate_workspace("true");
        degrade(workspace.path());

        let restored: std::sync::Arc<std::sync::Mutex<Option<(PathBuf, usize)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let recorder = restored.clone();
        let outcome = probe_most_overdue(
            &short_config(),
            &DefaultGatesConfig::default(),
            Duration::from_secs(30),
            move |workspace, gates_run| {
                let recorder = recorder.clone();
                async move {
                    *recorder.lock().unwrap() = Some((workspace, gates_run));
                    Ok(())
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(
            outcome,
            IdleProbeOutcome::Probed {
                workspace: workspace.path().to_path_buf(),
                verdict: ProbeVerdict::Passed { gates_run: 1 },
            }
        );
        assert_eq!(
            *restored.lock().unwrap(),
            Some((workspace.path().to_path_buf(), 1))
        );
        assert!(
            load_state(workspace.path()).unwrap().is_none(),
            "a passing probe must clear the probe schedule"
        );
    }

    #[tokio::test]
    async fn failing_probe_backs_off_and_is_not_immediately_due() {
        let (_guard, _home) = isolated_home();
        let workspace = gate_workspace("false");
        degrade(workspace.path());

        let outcome = probe_most_overdue(
            &short_config(),
            &DefaultGatesConfig::default(),
            Duration::from_secs(30),
            |_, _| async { Ok(()) },
        )
        .await
        .unwrap();
        let verdict = match &outcome {
            IdleProbeOutcome::Probed { verdict, .. } => verdict,
            other => panic!("expected a probe, got {other:?}"),
        };
        let ProbeVerdict::Failed { reason } = verdict else {
            panic!("expected a Failed verdict, got {verdict:?}")
        };
        assert!(!reason.is_empty());

        let state = load_state(workspace.path()).unwrap().unwrap();
        assert_eq!(state.backoff_secs, 5);
        assert_eq!(state.consecutive_probe_failures, 1);
        let next = DateTime::parse_from_rfc3339(&state.next_probe_at).unwrap();
        assert!(next.with_timezone(&Utc) > Utc::now());

        // The fresh backoff holds: an immediate second pass finds nothing due.
        let outcome = probe_most_overdue(
            &short_config(),
            &DefaultGatesConfig::default(),
            Duration::from_secs(30),
            |_, _| async { Ok(()) },
        )
        .await
        .unwrap();
        assert_eq!(outcome, IdleProbeOutcome::Idle);
    }

    #[tokio::test]
    async fn nothing_degraded_or_due_is_idle() {
        let (_guard, _home) = isolated_home();
        let outcome = probe_most_overdue(
            &short_config(),
            &DefaultGatesConfig::default(),
            Duration::from_secs(30),
            |_, _| async { Ok(()) },
        )
        .await
        .unwrap();
        assert_eq!(outcome, IdleProbeOutcome::Idle);
    }
}
