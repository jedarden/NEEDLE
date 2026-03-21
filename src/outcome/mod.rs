//! Outcome routing: map agent exit codes to explicit handlers.
//!
//! Every possible exit code has a named handler. The type system enforces
//! exhaustiveness — if an outcome can happen, it must have a handler.
//!
//! Depends on: `types`, `config`, `bead_store`, `telemetry`.

use anyhow::Result;

use crate::bead_store::BeadStore;
use crate::config::Config;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{AgentOutcome, BeadId, BeadStatus, Outcome};

// ──────────────────────────────────────────────────────────────────────────────
// BeadAction
// ──────────────────────────────────────────────────────────────────────────────

/// What happened to the bead after outcome handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeadAction {
    /// Bead was released back to open status.
    Released,
    /// Bead was released and marked as deferred.
    Deferred,
    /// Bead was released and an alert was created.
    Alerted,
    /// No bead state change (agent owns closure).
    None,
}

impl std::fmt::Display for BeadAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BeadAction::Released => write!(f, "released"),
            BeadAction::Deferred => write!(f, "deferred"),
            BeadAction::Alerted => write!(f, "alerted"),
            BeadAction::None => write!(f, "none"),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// HandlerResult
// ──────────────────────────────────────────────────────────────────────────────

/// The result of outcome handling — describes what happened and what was emitted.
#[derive(Debug)]
pub struct HandlerResult {
    /// The classified outcome.
    pub outcome: Outcome,
    /// What happened to the bead.
    pub bead_action: BeadAction,
}

// ──────────────────────────────────────────────────────────────────────────────
// Re-export classify for convenience
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
        bead_id: &BeadId,
        output: AgentOutcome,
        was_interrupted: bool,
    ) -> Result<HandlerResult> {
        let outcome = classify(output.exit_code, was_interrupted);

        let outcome_name = outcome_to_string(&outcome);
        self.telemetry.emit(EventKind::OutcomeClassified {
            bead_id: bead_id.clone(),
            outcome: outcome_name.clone(),
            exit_code: output.exit_code,
        })?;

        let bead_action = match outcome {
            Outcome::Success => self.handle_success(store, bead_id).await?,
            Outcome::Failure => self.handle_failure(store, bead_id).await?,
            Outcome::Timeout => self.handle_timeout(store, bead_id).await?,
            Outcome::AgentNotFound => self.handle_agent_not_found(store, bead_id).await?,
            Outcome::Interrupted => self.handle_interrupted(store, bead_id).await?,
            Outcome::Crash(code) => self.handle_crash(store, bead_id, code).await?,
        };

        self.telemetry.emit(EventKind::OutcomeHandled {
            bead_id: bead_id.clone(),
            outcome: outcome_name,
            action: bead_action.to_string(),
        })?;

        Ok(HandlerResult {
            outcome,
            bead_action,
        })
    }

    /// Success: verify bead was closed by agent. If still open, emit orphaned warning.
    /// NEEDLE does NOT auto-close — the agent owns closure via `br close`.
    async fn handle_success(&self, store: &dyn BeadStore, bead_id: &BeadId) -> Result<BeadAction> {
        tracing::info!(bead_id = %bead_id, "agent completed successfully");

        // Check if the agent closed the bead.
        match store.show(bead_id).await {
            Ok(bead) => {
                match bead.status {
                    BeadStatus::Done => {
                        tracing::info!(bead_id = %bead_id, "bead confirmed closed by agent");
                    }
                    BeadStatus::Open | BeadStatus::InProgress | BeadStatus::Blocked => {
                        // Agent exited 0 but didn't close the bead — orphaned.
                        tracing::warn!(
                            bead_id = %bead_id,
                            status = %bead.status,
                            "agent exited successfully but bead is still open (orphaned)"
                        );
                        self.telemetry.emit(EventKind::BeadOrphaned {
                            bead_id: bead_id.clone(),
                        })?;
                    }
                }
            }
            Err(e) => {
                // Can't verify — log warning but don't fail.
                tracing::warn!(
                    bead_id = %bead_id,
                    error = %e,
                    "could not verify bead closure status"
                );
            }
        }

        Ok(BeadAction::None)
    }

    /// Failure: evaluate for mitosis (Phase 2 stub). If not splittable, release
    /// and increment failure count label.
    async fn handle_failure(&self, store: &dyn BeadStore, bead_id: &BeadId) -> Result<BeadAction> {
        tracing::warn!(bead_id = %bead_id, "agent failure — releasing bead");

        // Phase 1 stub: mitosis always returns "not splittable".
        let splittable = false;

        if splittable {
            // Phase 2: split bead into children, block parent.
            unreachable!("mitosis not implemented in Phase 1");
        }

        // Release bead back to open.
        store.release(bead_id).await?;

        // Increment failure count label.
        self.increment_failure_count(store, bead_id).await?;

        self.telemetry.emit(EventKind::BeadReleased {
            bead_id: bead_id.clone(),
            reason: "failure".to_string(),
        })?;

        Ok(BeadAction::Released)
    }

    /// Timeout: release bead and add `deferred` label.
    async fn handle_timeout(&self, store: &dyn BeadStore, bead_id: &BeadId) -> Result<BeadAction> {
        tracing::warn!(bead_id = %bead_id, "agent timed out — releasing bead as deferred");

        store.release(bead_id).await?;

        // Add deferred label so we can track timeout patterns.
        if let Err(e) = store.add_label(bead_id, "deferred").await {
            tracing::warn!(bead_id = %bead_id, error = %e, "failed to add deferred label");
        }

        self.telemetry.emit(EventKind::BeadReleased {
            bead_id: bead_id.clone(),
            reason: "timeout".to_string(),
        })?;

        Ok(BeadAction::Deferred)
    }

    /// Crash: release bead and create alert with diagnostic info.
    async fn handle_crash(
        &self,
        store: &dyn BeadStore,
        bead_id: &BeadId,
        signal_code: i32,
    ) -> Result<BeadAction> {
        tracing::error!(
            bead_id = %bead_id,
            signal_code,
            agent = %self.config.agent.default,
            "agent crashed — releasing bead and creating alert"
        );

        store.release(bead_id).await?;

        // Add crash label for tracking.
        if let Err(e) = store.add_label(bead_id, "crash").await {
            tracing::warn!(bead_id = %bead_id, error = %e, "failed to add crash label");
        }

        self.telemetry.emit(EventKind::BeadReleased {
            bead_id: bead_id.clone(),
            reason: format!("crash (signal {})", signal_code),
        })?;

        // Phase 1: log the alert details. Phase 2 will create an actual alert bead
        // via BeadStore::create when that method is available.
        tracing::error!(
            bead_id = %bead_id,
            agent = %self.config.agent.default,
            signal_code,
            workspace = %self.config.workspace.default.display(),
            "ALERT: Agent crash — manual investigation required"
        );

        Ok(BeadAction::Alerted)
    }

    /// AgentNotFound: release bead, emit error. No retry — this is a config issue.
    async fn handle_agent_not_found(
        &self,
        store: &dyn BeadStore,
        bead_id: &BeadId,
    ) -> Result<BeadAction> {
        tracing::error!(
            bead_id = %bead_id,
            agent = %self.config.agent.default,
            "agent binary not found — releasing bead (config issue, no retry)"
        );

        store.release(bead_id).await?;

        self.telemetry.emit(EventKind::BeadReleased {
            bead_id: bead_id.clone(),
            reason: "agent_not_found".to_string(),
        })?;

        Ok(BeadAction::Released)
    }

    /// Interrupted: release bead for graceful shutdown.
    async fn handle_interrupted(
        &self,
        store: &dyn BeadStore,
        bead_id: &BeadId,
    ) -> Result<BeadAction> {
        tracing::info!(bead_id = %bead_id, "agent interrupted — releasing bead for clean shutdown");

        store.release(bead_id).await?;

        self.telemetry.emit(EventKind::BeadReleased {
            bead_id: bead_id.clone(),
            reason: "interrupted".to_string(),
        })?;

        Ok(BeadAction::Released)
    }

    /// Increment the failure count label on a bead.
    ///
    /// Labels follow the pattern `failure:N`. If `failure:2` exists, we add
    /// `failure:3`. If no failure label exists, we add `failure:1`.
    async fn increment_failure_count(&self, store: &dyn BeadStore, bead_id: &BeadId) -> Result<()> {
        let labels = match store.labels(bead_id).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(
                    bead_id = %bead_id,
                    error = %e,
                    "could not read labels to increment failure count"
                );
                return Ok(());
            }
        };

        let current_count = labels
            .iter()
            .filter_map(|l| l.strip_prefix("failure:"))
            .filter_map(|n| n.parse::<u32>().ok())
            .max()
            .unwrap_or(0);

        let new_label = format!("failure:{}", current_count + 1);
        if let Err(e) = store.add_label(bead_id, &new_label).await {
            tracing::warn!(
                bead_id = %bead_id,
                label = %new_label,
                error = %e,
                "failed to add failure count label"
            );
        }

        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Convert an `Outcome` to its string representation for telemetry.
fn outcome_to_string(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Success => "success".to_string(),
        Outcome::Failure => "failure".to_string(),
        Outcome::Timeout => "timeout".to_string(),
        Outcome::AgentNotFound => "agent_not_found".to_string(),
        Outcome::Interrupted => "interrupted".to_string(),
        Outcome::Crash(code) => format!("crash({})", code),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::Filters;
    use crate::types::{Bead, BeadId, BeadStatus, ClaimResult};
    use async_trait::async_trait;
    use chrono::Utc;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    // ── Mock BeadStore ──

    /// Actions recorded by the mock store.
    #[derive(Debug, Clone)]
    enum StoreAction {
        Release(String),
        AddLabel(String, String),
        Show(String),
    }

    struct MockBeadStore {
        actions: Arc<Mutex<Vec<StoreAction>>>,
        /// What status to return for show() calls.
        show_status: BeadStatus,
        /// Labels to return for labels() calls.
        labels: Vec<String>,
    }

    impl MockBeadStore {
        fn new(show_status: BeadStatus) -> Self {
            MockBeadStore {
                actions: Arc::new(Mutex::new(Vec::new())),
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
            workspace: PathBuf::from("/tmp/test"),
            dependencies: vec![],
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
        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("new-bead".to_string()))
        }
        async fn doctor_repair(&self) -> Result<crate::bead_store::RepairReport> {
            Ok(crate::bead_store::RepairReport::default())
        }
    }

    fn test_handler() -> OutcomeHandler {
        let config = Config::default();
        let telemetry = Telemetry::new("test-worker".to_string());
        OutcomeHandler::new(config, telemetry)
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
        // 130 is >128, so it's a crash (signal exit), not Interrupted.
        // Interrupted only comes from the was_interrupted flag.
        assert_eq!(classify(130, false), Outcome::Crash(130));
    }

    #[test]
    fn classify_no_wildcard_arms_on_outcome() {
        // Verify all exit code ranges produce explicit outcomes, matching
        // the actual Outcome::classify mapping in types.
        assert_eq!(classify(0, false), Outcome::Success);
        assert_eq!(classify(1, false), Outcome::Failure);
        assert_eq!(classify(2, false), Outcome::Failure);
        assert_eq!(classify(99, false), Outcome::Failure);
        assert_eq!(classify(100, false), Outcome::Failure); // 2..=123 range
        assert_eq!(classify(124, false), Outcome::Timeout);
        assert_eq!(classify(125, false), Outcome::Failure); // 125-126 are failure
        assert_eq!(classify(126, false), Outcome::Failure);
        assert_eq!(classify(127, false), Outcome::AgentNotFound);
        assert_eq!(classify(128, false), Outcome::Crash(128));
        assert_eq!(classify(130, false), Outcome::Crash(130)); // signal exits
        assert_eq!(classify(143, false), Outcome::Crash(143)); // SIGTERM
        assert_eq!(classify(-9, false), Outcome::Crash(-9));
        assert_eq!(classify(200, false), Outcome::Crash(200)); // >128
    }

    // ── handle tests ──

    #[tokio::test]
    async fn handle_success_bead_closed_by_agent() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::Done);
        let bead_id = BeadId::from("needle-test");
        let output = AgentOutcome {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        };

        let result = handler
            .handle(&store, &bead_id, output, false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::None);
        // No release should happen — agent closed the bead.
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
        let bead_id = BeadId::from("needle-test");
        let output = AgentOutcome {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        };

        let result = handler
            .handle(&store, &bead_id, output, false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::None);
        // Show was called to check status.
        let actions = store.actions();
        assert!(
            actions.iter().any(|a| matches!(a, StoreAction::Show(_))),
            "success should check bead status"
        );
    }

    #[tokio::test]
    async fn handle_failure_releases_and_increments_count() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead_id = BeadId::from("needle-test");
        let output = AgentOutcome {
            exit_code: 1,
            stdout: String::new(),
            stderr: String::new(),
        };

        let result = handler
            .handle(&store, &bead_id, output, false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Failure);
        assert_eq!(result.bead_action, BeadAction::Released);

        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "failure must release bead"
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::AddLabel(_, label) if label == "failure:1")),
            "failure must increment failure count"
        );
    }

    #[tokio::test]
    async fn handle_failure_increments_existing_count() {
        let handler = test_handler();
        let store =
            MockBeadStore::new(BeadStatus::InProgress).with_labels(vec!["failure:2".to_string()]);
        let bead_id = BeadId::from("needle-test");
        let output = AgentOutcome {
            exit_code: 1,
            stdout: String::new(),
            stderr: String::new(),
        };

        let result = handler
            .handle(&store, &bead_id, output, false)
            .await
            .unwrap();

        assert_eq!(result.bead_action, BeadAction::Released);
        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::AddLabel(_, label) if label == "failure:3")),
            "should increment to failure:3"
        );
    }

    #[tokio::test]
    async fn handle_timeout_releases_and_adds_deferred() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead_id = BeadId::from("needle-test");
        let output = AgentOutcome {
            exit_code: 124,
            stdout: String::new(),
            stderr: String::new(),
        };

        let result = handler
            .handle(&store, &bead_id, output, false)
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
    async fn handle_crash_releases_and_alerts() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead_id = BeadId::from("needle-test");
        let output = AgentOutcome {
            exit_code: -9,
            stdout: String::new(),
            stderr: String::new(),
        };

        let result = handler
            .handle(&store, &bead_id, output, false)
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Crash(-9));
        assert_eq!(result.bead_action, BeadAction::Alerted);

        let actions = store.actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::Release(id) if id == "needle-test")),
            "crash must release bead"
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StoreAction::AddLabel(_, label) if label == "crash")),
            "crash must add crash label"
        );
    }

    #[tokio::test]
    async fn handle_agent_not_found_releases() {
        let handler = test_handler();
        let store = MockBeadStore::new(BeadStatus::InProgress);
        let bead_id = BeadId::from("needle-test");
        let output = AgentOutcome {
            exit_code: 127,
            stdout: String::new(),
            stderr: String::new(),
        };

        let result = handler
            .handle(&store, &bead_id, output, false)
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
        let bead_id = BeadId::from("needle-test");
        let output = AgentOutcome {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        };

        // was_interrupted = true overrides exit code.
        let result = handler
            .handle(&store, &bead_id, output, true)
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

    #[test]
    fn outcome_to_string_covers_all_variants() {
        // Verify all variants produce reasonable strings.
        assert_eq!(outcome_to_string(&Outcome::Success), "success");
        assert_eq!(outcome_to_string(&Outcome::Failure), "failure");
        assert_eq!(outcome_to_string(&Outcome::Timeout), "timeout");
        assert_eq!(
            outcome_to_string(&Outcome::AgentNotFound),
            "agent_not_found"
        );
        assert_eq!(outcome_to_string(&Outcome::Interrupted), "interrupted");
        assert_eq!(outcome_to_string(&Outcome::Crash(-9)), "crash(-9)");
    }

    #[test]
    fn bead_action_display() {
        assert_eq!(BeadAction::Released.to_string(), "released");
        assert_eq!(BeadAction::Deferred.to_string(), "deferred");
        assert_eq!(BeadAction::Alerted.to_string(), "alerted");
        assert_eq!(BeadAction::None.to_string(), "none");
    }

    #[test]
    fn handler_result_contains_both_fields() {
        let result = HandlerResult {
            outcome: Outcome::Success,
            bead_action: BeadAction::None,
        };
        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.bead_action, BeadAction::None);
    }
}
