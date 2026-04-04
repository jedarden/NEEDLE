//! Outcome routing: map agent exit codes to explicit handlers.
//!
//! Every possible exit code has a named handler. The type system enforces
//! exhaustiveness — if an outcome can happen, it must have a handler.
//!
//! Depends on: `types`, `config`, `bead_store`, `telemetry`, `validation`.

use std::fmt;

use anyhow::{Context, Result};
use chrono::Utc;

use crate::bead_store::BeadStore;
use crate::config::Config;
use crate::telemetry::{EventKind, Telemetry};
#[cfg(test)]
use crate::types::BeadStatus;
use crate::types::{AgentOutcome, Bead, BeadAction, HandlerResult, Outcome};
use crate::validation::{GateConfig, ValidationGate};

// ──────────────────────────────────────────────────────────────────────────────
// classify (convenience re-export)
// ──────────────────────────────────────────────────────────────────────────────

/// Classify an agent exit code into an `Outcome`, with shutdown signal support.
///
/// Delegates to `Outcome::classify()`. Provided here for ergonomic imports.
pub fn classify(exit_code: i32, was_interrupted: bool) -> Outcome {
    Outcome::classify(exit_code, was_interrupted)
}

// ──────────────────────────────────────────────────────────────────────────────
// OutcomeHandler
// ──────────────────────────────────────────────────────────────────────────────

/// Routes agent outcomes to their explicit handlers.
pub struct OutcomeHandler {
    config: Config,
    telemetry: Telemetry,
}

impl OutcomeHandler {
    pub fn new(config: Config, telemetry: Telemetry) -> Self {
        OutcomeHandler { config, telemetry }
    }

    /// Handle a process output for the given bead.
    ///
    /// Uses `classify()` to determine the outcome, then dispatches to the
    /// per-outcome handler. Every `Outcome` variant has an explicit arm —
    /// no wildcards. Returns a `HandlerResult` describing what happened.
    pub async fn handle(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        output: &AgentOutcome,
        was_interrupted: bool,
    ) -> Result<HandlerResult> {
        let outcome = classify(output.exit_code, was_interrupted);

        tracing::info!(
            bead_id = %bead.id,
            exit_code = output.exit_code,
            outcome = %outcome,
            "handling agent outcome"
        );

        self.telemetry.emit(EventKind::OutcomeClassified {
            bead_id: bead.id.clone(),
            outcome: outcome.as_str().to_string(),
            exit_code: output.exit_code,
        })?;

        let (bead_action, telemetry_events) = match outcome.clone() {
            Outcome::Success => self.handle_success(store, bead).await?,
            Outcome::Failure => self.handle_failure(store, bead).await?,
            Outcome::Timeout => self.handle_timeout(store, bead).await?,
            Outcome::AgentNotFound => self.handle_agent_not_found(store, bead).await?,
            Outcome::Interrupted => self.handle_interrupted(store, bead).await?,
            Outcome::Crash(code) => self.handle_crash(store, bead, code).await?,
        };

        // Emit sub-handler events (e.g. BeadCompleted, BeadOrphaned) to the
        // telemetry sink so they appear in the JSONL log.
        for event in &telemetry_events {
            self.telemetry.emit(event.clone())?;
        }

        self.telemetry.emit(EventKind::OutcomeHandled {
            bead_id: bead.id.clone(),
            outcome: outcome.as_str().to_string(),
            action: bead_action.to_string(),
        })?;

        Ok(HandlerResult {
            outcome,
            bead_action,
            telemetry_events,
        })
    }

    /// Success: run validation gates (if configured), then verify bead closure.
    ///
    /// Flow:
    /// 1. If validation gates are configured (new or legacy format), run them.
    /// 2. If any gate fails: reopen the bead (if agent closed it) and release it.
    /// 3. If all gates pass (or none configured): check if agent closed the bead.
    ///    - Closed → emit BeadCompleted.
    ///    - Still open → emit BeadOrphaned warning.
    ///
    /// NEEDLE does NOT auto-close — the agent owns closure via `br close`.
    async fn handle_success(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::info!(bead_id = %bead.id, "agent completed successfully");

        // Try pluggable gates first, fall back to legacy verification commands.
        let gate = if !self.config.gates.is_empty() {
            // New pluggable gate system.
            let gate_configs: Vec<(String, GateConfig)> = self
                .config
                .gates
                .iter()
                .enumerate()
                .map(|(i, config)| (format!("gate_{}", i), config.clone()))
                .collect();
            ValidationGate::new(gate_configs, bead.workspace.clone())
        } else if !self.config.verification.is_empty() {
            // Legacy verification command format.
            ValidationGate::from_commands(self.config.verification.clone(), bead.workspace.clone())
        } else {
            None
        };

        if let Some(gate) = gate {
            let report = gate.run(bead).await?;

            if !report.all_passed {
                return self.handle_gate_failure(store, bead, &report).await;
            }

            // All gates passed — emit telemetry.
            let gates_run = report.results.len() as u32;
            self.telemetry.emit(EventKind::VerificationPassed {
                bead_id: bead.id.clone(),
                gates_run,
            })?;
            tracing::info!(
                bead_id = %bead.id,
                gates_run,
                "all validation gates passed"
            );
        }

        // Normal success flow: check if agent closed the bead.
        let mut events = Vec::new();

        match store.show(&bead.id).await {
            Ok(current) if current.status.is_done() => {
                tracing::info!(bead_id = %bead.id, "bead confirmed closed by agent");
                events.push(EventKind::BeadCompleted {
                    bead_id: bead.id.clone(),
                    duration_ms: 0,
                });
                // Increment success_count for any skills that matched this bead.
                if !bead.workspace.as_os_str().is_empty() {
                    if let Ok(lib) = crate::skill::SkillLibrary::load(&bead.workspace) {
                        if let Err(e) = lib.increment_success_for_bead(&bead.labels, &bead.title) {
                            tracing::warn!(
                                bead_id = %bead.id,
                                error = %e,
                                "failed to increment skill success counts"
                            );
                        }
                    }
                }
            }
            Ok(current) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    status = %current.status,
                    "agent exited successfully but bead is still open (orphaned)"
                );
                events.push(EventKind::BeadOrphaned {
                    bead_id: bead.id.clone(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "could not verify bead closure status"
                );
            }
        }

        Ok((BeadAction::None, events))
    }

    /// Handle gate failure: reopen the bead if it was closed, then release it.
    async fn handle_gate_failure(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        report: &crate::validation::GateReport,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        // Find the first failing gate for telemetry.
        let (failed_gate, reason) = report
            .results
            .iter()
            .find(|(_, r)| !r.passed())
            .map(|(name, r)| (name.clone(), r.failure_reason().unwrap_or("").to_string()))
            .unwrap_or_else(|| ("unknown".to_string(), "unknown error".to_string()));

        tracing::warn!(
            bead_id = %bead.id,
            gate = %failed_gate,
            reason = %reason,
            "validation gate failed — releasing bead"
        );

        // Emit verification failure telemetry.
        self.telemetry.emit(EventKind::VerificationFailed {
            bead_id: bead.id.clone(),
            command: failed_gate.clone(),
            exit_code: None,
            output: reason,
        })?;

        // If the agent already closed the bead, reopen it before releasing.
        match store.show(&bead.id).await {
            Ok(current) if current.status.is_done() => {
                tracing::info!(
                    bead_id = %bead.id,
                    "reopening bead closed by agent (verification failed)"
                );
                if let Err(e) = store.reopen(&bead.id).await {
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %e,
                        "failed to reopen bead — attempting release anyway"
                    );
                }
            }
            _ => {}
        }

        // Release the bead back to open.
        store.release(&bead.id).await?;
        self.increment_failure_count(store, bead).await?;

        // Add a label indicating verification failure.
        if let Err(e) = store.add_label(&bead.id, "verification-failed").await {
            tracing::warn!(
                bead_id = %bead.id,
                error = %e,
                "failed to add verification-failed label"
            );
        }

        let event = EventKind::BeadReleased {
            bead_id: bead.id.clone(),
            reason: format!("verification_failed: {}", failed_gate),
        };
        self.telemetry.emit(event.clone())?;

        Ok((BeadAction::Released, vec![event]))
    }

    /// Failure: release bead and increment failure count.
    ///
    /// Mitosis evaluation (for multi-task splitting) is handled externally by
    /// the worker after outcome handling — see `MitosisEvaluator`.
    async fn handle_failure(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::warn!(bead_id = %bead.id, "agent failure — releasing bead");

        store.release(&bead.id).await?;
        self.increment_failure_count(store, bead).await?;

        let event = EventKind::BeadReleased {
            bead_id: bead.id.clone(),
            reason: "failure".to_string(),
        };
        self.telemetry.emit(event.clone())?;

        Ok((BeadAction::Released, vec![event]))
    }

    /// Timeout: release bead and add `deferred` label.
    async fn handle_timeout(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::warn!(bead_id = %bead.id, "agent timed out — releasing bead as deferred");

        store.release(&bead.id).await?;

        if let Err(e) = store.add_label(&bead.id, "deferred").await {
            tracing::warn!(bead_id = %bead.id, error = %e, "failed to add deferred label");
        }

        let event = EventKind::BeadReleased {
            bead_id: bead.id.clone(),
            reason: "timeout".to_string(),
        };
        self.telemetry.emit(event.clone())?;

        Ok((BeadAction::Deferred, vec![event]))
    }

    /// Crash: release bead and create alert bead with diagnostic info.
    async fn handle_crash(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        signal_code: i32,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::error!(
            bead_id = %bead.id,
            signal_code,
            agent = %self.config.agent.default,
            "agent crashed — releasing bead and creating alert"
        );

        store.release(&bead.id).await?;

        // Create alert bead with diagnostic info.
        let signal_num = if signal_code > 128 {
            signal_code - 128
        } else {
            signal_code
        };
        let timestamp = Utc::now().to_rfc3339();
        let alert_title = format!("ALERT: Agent crash on bead {}", bead.id);
        let alert_body = format!(
            "## Agent Crash Report\n\
             \n\
             - **Bead ID**: {}\n\
             - **Agent**: {}\n\
             - **Exit code**: {} (signal {})\n\
             - **Workspace**: {}\n\
             - **Timestamp**: {}\n\
             \n\
             The agent process was killed. This bead has been released for retry.",
            bead.id,
            self.config.agent.default,
            signal_code,
            signal_num,
            bead.workspace.display(),
            timestamp,
        );

        let alert_labels = ["alert", "crash", &format!("signal-{}", signal_num)];
        match store
            .create_bead(&alert_title, &alert_body, &alert_labels)
            .await
        {
            Ok(alert_id) => {
                tracing::info!(
                    bead_id = %bead.id,
                    %alert_id,
                    "crash alert bead created"
                );
            }
            Err(e) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "failed to create crash alert bead"
                );
            }
        }

        let event = EventKind::BeadReleased {
            bead_id: bead.id.clone(),
            reason: format!("crash_signal_{}", signal_code),
        };
        self.telemetry.emit(event.clone())?;

        Ok((BeadAction::Alerted, vec![event]))
    }

    /// AgentNotFound: release bead, emit error. No retry — this is a config issue.
    async fn handle_agent_not_found(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::error!(
            bead_id = %bead.id,
            agent = %self.config.agent.default,
            "agent binary not found — releasing bead (config issue, no retry)"
        );

        store.release(&bead.id).await?;

        let event = EventKind::BeadReleased {
            bead_id: bead.id.clone(),
            reason: "agent_not_found".to_string(),
        };
        self.telemetry.emit(event.clone())?;

        Ok((BeadAction::Released, vec![event]))
    }

    /// Interrupted: release bead for graceful shutdown.
    async fn handle_interrupted(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
    ) -> Result<(BeadAction, Vec<EventKind>)> {
        tracing::info!(bead_id = %bead.id, "agent interrupted — releasing bead for clean shutdown");

        store.release(&bead.id).await?;

        let event = EventKind::BeadReleased {
            bead_id: bead.id.clone(),
            reason: "interrupted".to_string(),
        };
        self.telemetry.emit(event.clone())?;

        Ok((BeadAction::Released, vec![event]))
    }

    /// Increment the failure count label on a bead.
    ///
    /// Labels follow the pattern `failure-count:N`. If `failure-count:2` exists,
    /// the old label is removed and `failure-count:3` is added.
    async fn increment_failure_count(&self, store: &dyn BeadStore, bead: &Bead) -> Result<()> {
        let labels = match store.labels(&bead.id).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    error = %e,
                    "could not read labels to increment failure count"
                );
                return Ok(());
            }
        };

        let current_count = labels
            .iter()
            .filter_map(|l| l.strip_prefix("failure-count:"))
            .filter_map(|n| n.parse::<u32>().ok())
            .max()
            .unwrap_or(0);

        let new_count = current_count + 1;
        let new_label = format!("failure-count:{}", new_count);

        // Remove old failure-count labels before adding the new one.
        for label in &labels {
            if label.starts_with("failure-count:") {
                if let Err(e) = store.remove_label(&bead.id, label).await {
                    tracing::warn!(
                        bead_id = %bead.id,
                        label,
                        error = %e,
                        "failed to remove old failure-count label"
                    );
                }
            }
        }

        store
            .add_label(&bead.id, &new_label)
            .await
            .with_context(|| format!("failed to add label {} to bead {}", new_label, bead.id))?;

        tracing::debug!(
            bead_id = %bead.id,
            count = new_count,
            "failure count incremented"
        );

        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Outcome Display
// ──────────────────────────────────────────────────────────────────────────────

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Success => write!(f, "Success"),
            Outcome::Failure => write!(f, "Failure"),
            Outcome::Timeout => write!(f, "Timeout"),
            Outcome::AgentNotFound => write!(f, "AgentNotFound"),
            Outcome::Interrupted => write!(f, "Interrupted"),
            Outcome::Crash(code) => write!(f, "Crash({})", code),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::Filters;
    use crate::telemetry::TelemetrySink;
    use crate::types::{BeadId, ClaimResult};
    use async_trait::async_trait;
    use chrono::Utc;
    use std::path::PathBuf;
    use std::sync::Mutex;

    // ── Mock BeadStore ──

    #[derive(Debug, Clone)]
    #[allow(dead_code)] // Fields read via pattern matching in test assertions
    enum StoreAction {
        Release(String),
        Reopen(String),
        AddLabel(String, String),
        RemoveLabel(String, String),
        Show(String),
        CreateBead(String, String),
        AddDependency(String, String),
    }

    struct MockBeadStore {
        actions: Mutex<Vec<StoreAction>>,
        show_status: BeadStatus,
        labels: Vec<String>,
    }

    impl MockBeadStore {
        fn new(show_status: BeadStatus) -> Self {
            MockBeadStore {
                actions: Mutex::new(Vec::new()),
                show_status,
                labels: Vec::new(),
            }
        }

        fn with_labels(mut self, labels: Vec<String>) -> Self {
            self.labels = labels;
            self
        }

        fn actions(&self) -> Vec<StoreAction> {
            self.actions.lock().unwrap().clone()
        }
    }

    fn test_bead(status: BeadStatus) -> Bead {
        Bead {
            id: BeadId::from("needle-test"),
            title: "Test bead".to_string(),
            body: Some("Test body".to_string()),
            priority: 1,
            status,
            assignee: Some("worker-01".to_string()),
            labels: vec![],
            workspace: PathBuf::from("/tmp"),
            dependencies: vec![],
            dependents: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[async_trait]
    impl BeadStore for MockBeadStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn show(&self, id: &BeadId) -> Result<Bead> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::Show(id.to_string()));
            Ok(test_bead(self.show_status.clone()))
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "mock".to_string(),
            })
        }
        async fn release(&self, id: &BeadId) -> Result<()> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::Release(id.to_string()));
            Ok(())
        }
        async fn reopen(&self, id: &BeadId) -> Result<()> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::Reopen(id.to_string()));
            Ok(())
        }
        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            Ok(self.labels.clone())
        }
        async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::AddLabel(id.to_string(), label.to_string()));
            Ok(())
        }
        async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::RemoveLabel(id.to_string(), label.to_string()));
            Ok(())
        }
        async fn create_bead(&self, title: &str, body: &str, _labels: &[&str]) -> Result<BeadId> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::CreateBead(title.to_string(), body.to_string()));
            Ok(BeadId::from("alert-001"))
        }
        async fn doctor_repair(&self) -> Result<crate::bead_store::RepairReport> {
            Ok(crate::bead_store::RepairReport::default())
        }
        async fn doctor_check(&self) -> Result<crate::bead_store::RepairReport> {
            Ok(crate::bead_store::RepairReport::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
        async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
            self.actions
                .lock()
                .unwrap()
                .push(StoreAction::AddDependency(
                    blocker_id.to_string(),
                    blocked_id.to_string(),
                ));
            Ok(())
        }
    }

    struct NopSink;

    impl TelemetrySink for NopSink {
        fn write(&self, _event: &crate::telemetry::TelemetryEvent) -> Result<()> {
            Ok(())
        }
        fn flush(&self) -> Result<()> {
            Ok(())
        }
    }

    fn test_handler() -> OutcomeHandler {
        let config = Config::default();
        let telemetry = Telemetry::with_sink("test-worker".to_string(), NopSink);
        OutcomeHandler::new(config, telemetry)
    }

    fn test_output(exit_code: i32) -> AgentOutcome {
        AgentOutcome {
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    // ── classify tests ──

    #[test]
    fn classify_was_interrupted_always_returns_interrupted() {
        assert_eq!(classify(0, true), Outcome::Interrupted);
        assert_eq!(classify(1, true), Outcome::Interrupted);
        assert_eq!(classify(127, true), Outcome::Interrupted);
    }

    #[test]
    fn classify_not_interrupted_uses_exit_code() {
        assert_eq!(classify(0, false), Outcome::Success);
        assert_eq!(classify(1, false), Outcome::Failure);
        assert_eq!(classify(124, false), Outcome::Timeout);
        assert_eq!(classify(127, false), Outcome::AgentNotFound);
        assert_eq!(classify(129, false), Outcome::Crash(129));
    }

    #[test]
    fn classify_no_wildcard_arms() {
        // Verify key exit codes map correctly per spec.
        assert_eq!(classify(0, false), Outcome::Success);
        assert_eq!(classify(1, false), Outcome::Failure);
        assert_eq!(classify(2, false), Outcome::Failure);
        assert_eq!(classify(99, false), Outcome::Failure);
        assert_eq!(classify(100, false), Outcome::Failure);
        assert_eq!(classify(124, false), Outcome::Timeout);
        assert_eq!(classify(125, false), Outcome::Failure);
        assert_eq!(classify(128, false), Outcome::Failure); // not >128 per spec
        assert_eq!(classify(129, false), Outcome::Crash(129));
        assert_eq!(classify(137, false), Outcome::Crash(137));
        assert_eq!(classify(-9, false), Outcome::Crash(-9));
    }

    // ── handle tests ──

    #[tokio::test]
    async fn handle_success_bead_closed_by_agent() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::Done);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::None);
        assert!(!result.telemetry_events.is_empty());
        let actions = store.actions();
        assert!(
            !actions.iter().any(|a| matches!(a, StoreAction::Release(_))),
            "success should not release bead"
        );
    }

    #[tokio::test]
    async fn handle_success_bead_still_open_emits_orphaned() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::None);
        let actions = store.actions();
        assert!(
            actions.iter().any(|a| matches!(a, StoreAction::Show(_))),
            "success should check bead status"
        );
        assert!(result
            .telemetry_events
            .iter()
            .any(|e| matches!(e, EventKind::BeadOrphaned { .. })));
    }

    #[tokio::test]
    async fn handle_failure_releases_and_increments_count() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(1), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Failure);
        assert_eq!(result.bead_action, BeadAction::Released);
        assert!(!result.telemetry_events.is_empty());

        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "failure must release bead"
        );
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "failure-count:1")
            ),
            "failure must add failure-count:1"
        );
    }

    #[tokio::test]
    async fn handle_failure_increments_existing_count() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress)
            .with_labels(vec!["failure-count:2".to_string()]);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(1), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Released);
        let actions = store.actions();
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::RemoveLabel(_, label) if label == "failure-count:2")
            ),
            "should remove old failure-count label"
        );
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "failure-count:3")
            ),
            "should add failure-count:3"
        );
    }

    #[tokio::test]
    async fn handle_timeout_releases_and_adds_deferred() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(124), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Timeout);
        assert_eq!(result.bead_action, BeadAction::Deferred);

        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "timeout must release bead"
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::AddLabel(_, label) if label == "deferred")),
            "timeout must add deferred label"
        );
    }

    #[tokio::test]
    async fn handle_crash_releases_and_creates_alert_bead() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(137), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Crash(137));
        assert_eq!(result.bead_action, BeadAction::Alerted);

        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "crash must release bead"
        );
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::CreateBead(title, _) if title.contains("needle-test"))
            ),
            "crash must create alert bead referencing the original bead"
        );
    }

    #[tokio::test]
    async fn handle_crash_negative_exit_code() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(-1), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Crash(-1));
        assert_eq!(result.bead_action, BeadAction::Alerted);
    }

    #[tokio::test]
    async fn handle_agent_not_found_releases() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(127), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::AgentNotFound);
        assert_eq!(result.bead_action, BeadAction::Released);

        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "agent_not_found must release bead"
        );
    }

    #[tokio::test]
    async fn handle_interrupted_releases() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), true)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Interrupted);
        assert_eq!(result.bead_action, BeadAction::Released);

        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "interrupted must release bead"
        );
    }

    #[tokio::test]
    async fn handle_failure_emits_telemetry_events() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(2), false)
            .await
            .unwrap();

        assert!(
            result
                .telemetry_events
                .iter()
                .any(|e| matches!(e, EventKind::BeadReleased { .. })),
            "failure should emit BeadReleased event"
        );
    }

    #[test]
    fn outcome_display_covers_all_variants() {
        assert_eq!(format!("{}", Outcome::Success), "Success");
        assert_eq!(format!("{}", Outcome::Failure), "Failure");
        assert_eq!(format!("{}", Outcome::Timeout), "Timeout");
        assert_eq!(format!("{}", Outcome::AgentNotFound), "AgentNotFound");
        assert_eq!(format!("{}", Outcome::Interrupted), "Interrupted");
        assert_eq!(format!("{}", Outcome::Crash(-9)), "Crash(-9)");
    }

    // ── verification gate tests ──

    fn test_handler_with_verification(commands: Vec<String>) -> OutcomeHandler {
        let config = Config {
            verification: commands,
            ..Config::default()
        };
        let telemetry = Telemetry::with_sink("test-worker".to_string(), NopSink);
        OutcomeHandler::new(config, telemetry)
    }

    #[tokio::test]
    async fn handle_success_no_verification_default_behavior() {
        // No verification configured → normal success flow (unchanged behavior).
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::Done);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::None);
    }

    #[tokio::test]
    async fn handle_success_verification_passes_accepts_closure() {
        // Verification passes → bead closure accepted.
        let handler = test_handler_with_verification(vec!["true".to_string()]);
        let store = MockBeadStore::new(BeadStatus::Done);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::None);
        assert!(result
            .telemetry_events
            .iter()
            .any(|e| matches!(e, EventKind::BeadCompleted { .. })));
    }

    #[tokio::test]
    async fn handle_success_verification_fails_releases_bead() {
        // Verification fails → bead released.
        let handler = test_handler_with_verification(vec!["false".to_string()]);
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::Released);

        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "verification failure must release bead"
        );
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "verification-failed")
            ),
            "verification failure must add verification-failed label"
        );
    }

    #[tokio::test]
    async fn handle_success_verification_fails_reopens_closed_bead() {
        // Agent closed the bead, but verification fails → reopen then release.
        let handler = test_handler_with_verification(vec!["false".to_string()]);
        let store = MockBeadStore::new(BeadStatus::Done);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Released);

        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Reopen(id) if id == "needle-test")),
            "verification failure on closed bead must reopen it first"
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "verification failure must release bead after reopening"
        );
    }

    #[tokio::test]
    async fn handle_success_verification_fails_increments_failure_count() {
        let handler = test_handler_with_verification(vec!["false".to_string()]);
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let _result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        let actions = store.actions();
        assert!(
            actions.iter().any(
                |a| matches!(a, StoreAction::AddLabel(_, label) if label == "failure-count:1")
            ),
            "verification failure must increment failure count"
        );
    }

    #[tokio::test]
    async fn handle_success_multiple_gates_first_fails() {
        // First gate passes, second fails → should stop and release.
        let handler = test_handler_with_verification(vec![
            "true".to_string(),
            "false".to_string(),
            "echo should-not-run".to_string(),
        ]);
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead = test_bead(BeadStatus::InProgress);

        let result = handler
            .handle(&store, &bead, &test_output(0), false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Released);
    }
}
