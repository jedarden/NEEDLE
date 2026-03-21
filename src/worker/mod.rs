//! Worker loop: the core NEEDLE state machine.
//!
//! The Worker executes the strand waterfall, claims beads, dispatches the
//! agent, handles outcomes, and emits telemetry for every transition.
//!
//! State transitions are explicit — there is no implicit fallthrough and no
//! state that does not have a defined handler. The worker emits telemetry for
//! every transition.
//!
//! Depends on: `strand`, `claim`, `prompt`, `dispatch`, `outcome`,
//!             `bead_store`, `telemetry`, `health`, `config`, `types`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Result};

use crate::bead_store::BeadStore;
use crate::claim::Claimer;
use crate::config::{Config, ConfigLoader};
use crate::dispatch::Dispatcher;
use crate::health::HealthMonitor;
use crate::outcome::OutcomeHandler;
use crate::prompt::{BuiltPrompt, PromptBuilder};
use crate::strand::StrandRunner;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{AgentOutcome, Bead, BeadId, ClaimResult, IdleAction, WorkerState};

/// The NEEDLE worker — owns and drives the full state machine.
pub struct Worker {
    config: Config,
    worker_name: String,
    store: Arc<dyn BeadStore>,
    telemetry: Telemetry,
    strands: StrandRunner,
    claimer: Claimer,
    prompt_builder: PromptBuilder,
    dispatcher: Dispatcher,
    outcome_handler: OutcomeHandler,
    health: HealthMonitor,

    // State machine fields
    state: WorkerState,
    current_bead: Option<Bead>,
    exclusion_set: HashSet<BeadId>,
    retry_count: u32,
    beads_processed: u64,
    shutdown: Arc<AtomicBool>,
    last_error: Option<anyhow::Error>,
    boot_time: Option<Instant>,

    // Transient fields — pass data between state handlers within a single cycle.
    built_prompt: Option<BuiltPrompt>,
    exec_output: Option<(AgentOutcome, bool)>,
}

impl Worker {
    /// Construct a worker from config, a worker name, and a bead store implementation.
    pub fn new(config: Config, worker_name: String, store: Arc<dyn BeadStore>) -> Self {
        let telemetry = Telemetry::new(worker_name.clone());
        let strands = StrandRunner::from_config(&config);
        let claimer = Claimer::new(
            store.clone(),
            std::path::PathBuf::from("/tmp"),
            config.worker.max_claim_retries,
            100,
            Telemetry::new(worker_name.clone()),
        );
        let prompt_builder = PromptBuilder::new(&config.prompt);
        let dispatcher = match Dispatcher::new(&config, Telemetry::new(worker_name.clone())) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load adapters, using built-in defaults");
                let builtins = crate::dispatch::builtin_adapters()
                    .into_iter()
                    .map(|a| (a.name.clone(), a))
                    .collect();
                Dispatcher::with_adapters(
                    builtins,
                    Telemetry::new(worker_name.clone()),
                    config.agent.timeout,
                )
            }
        };
        let outcome_handler =
            OutcomeHandler::new(config.clone(), Telemetry::new(worker_name.clone()));
        let health = HealthMonitor::new(
            config.clone(),
            worker_name.clone(),
            Telemetry::new(worker_name.clone()),
        );

        Worker {
            config,
            worker_name,
            store,
            telemetry,
            strands,
            claimer,
            prompt_builder,
            dispatcher,
            outcome_handler,
            health,
            state: WorkerState::Booting,
            current_bead: None,
            exclusion_set: HashSet::new(),
            retry_count: 0,
            beads_processed: 0,
            shutdown: Arc::new(AtomicBool::new(false)),
            last_error: None,
            boot_time: None,
            built_prompt: None,
            exec_output: None,
        }
    }

    /// Run the worker loop until exhausted, stopped, or errored.
    ///
    /// The main loop is a match on `self.state`. Every state has a handler
    /// that performs its actions and sets `self.state` to the next state.
    pub async fn run(&mut self) -> Result<WorkerState> {
        // Boot: validate config and initialize.
        self.boot()?;

        // Install signal handlers.
        self.install_signal_handlers();

        loop {
            // Check for shutdown signal between states.
            if self.shutdown.load(Ordering::SeqCst) {
                match self.state {
                    // If we're in the middle of processing a bead, handle it
                    // as interrupted so the bead gets released.
                    WorkerState::Building
                    | WorkerState::Dispatching
                    | WorkerState::Executing
                    | WorkerState::Handling => {
                        // Let the current state handler finish, but mark
                        // that we should stop after handling.
                    }
                    // For states where we don't hold a bead, stop immediately.
                    WorkerState::Selecting
                    | WorkerState::Claiming
                    | WorkerState::Retrying
                    | WorkerState::Logging => {
                        // Release any claimed bead before stopping.
                        if let Some(ref bead) = self.current_bead {
                            let bead_id = bead.id.clone();
                            tracing::info!(bead_id = %bead_id, "releasing bead on shutdown");
                            let _ = self.store.release(&bead_id).await;
                        }
                        return self.stop("signal received").await;
                    }
                    WorkerState::Stopped | WorkerState::Exhausted | WorkerState::Errored => {
                        return self.stop("signal received").await;
                    }
                    WorkerState::Booting => {
                        return self.stop("signal received during boot").await;
                    }
                }
            }

            match self.state {
                WorkerState::Selecting => self.do_select().await?,
                WorkerState::Claiming => self.do_claim().await?,
                WorkerState::Retrying => self.do_retry()?,
                WorkerState::Building => self.do_build()?,
                WorkerState::Dispatching => self.do_dispatch().await?,
                WorkerState::Executing => self.do_execute().await?,
                WorkerState::Handling => self.do_handle().await?,
                WorkerState::Logging => self.do_log()?,
                WorkerState::Exhausted => {
                    return self.handle_exhausted().await;
                }
                WorkerState::Stopped => {
                    return self.stop("normal shutdown").await;
                }
                WorkerState::Errored => {
                    let err = self
                        .last_error
                        .take()
                        .unwrap_or_else(|| anyhow::anyhow!("unknown error"));
                    let msg = format!("{err}");
                    self.telemetry.emit(EventKind::WorkerErrored {
                        error_type: "worker_scoped".to_string(),
                        error_message: msg.clone(),
                        beads_processed: self.beads_processed,
                    })?;
                    return Err(err);
                }
                WorkerState::Booting => {
                    bail!("boot() should have transitioned past Booting state");
                }
            }
        }
    }

    // ── Boot ────────────────────────────────────────────────────────────────

    /// Validate configuration and initialize the worker.
    fn boot(&mut self) -> Result<()> {
        self.boot_time = Some(Instant::now());

        // Validate configuration.
        let errors = ConfigLoader::validate(&self.config);
        if !errors.is_empty() {
            let msg = errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            bail!("config validation failed: {msg}");
        }

        // Emit worker started event.
        self.telemetry.emit(EventKind::WorkerStarted {
            worker_name: self.worker_name.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        })?;

        self.set_state(WorkerState::Selecting)?;

        tracing::info!(
            worker = %self.worker_name,
            strands = ?self.strands.strand_names(),
            "worker booted"
        );

        Ok(())
    }

    // ── Signal handling ─────────────────────────────────────────────────────

    /// Install SIGTERM and SIGINT handlers that set the shutdown flag.
    fn install_signal_handlers(&self) {
        let shutdown = self.shutdown.clone();

        // SIGINT (Ctrl-C)
        let shutdown_int = shutdown.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!("received SIGINT, initiating graceful shutdown");
                shutdown_int.store(true, Ordering::SeqCst);
            }
        });

        // SIGTERM (Unix only)
        #[cfg(unix)]
        {
            let shutdown_term = shutdown;
            tokio::spawn(async move {
                let mut signal =
                    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to install SIGTERM handler");
                            return;
                        }
                    };
                signal.recv().await;
                tracing::info!("received SIGTERM, initiating graceful shutdown");
                shutdown_term.store(true, Ordering::SeqCst);
            });
        }
    }

    // ── State handlers ──────────────────────────────────────────────────────

    /// SELECTING: run strand waterfall to find a candidate bead.
    async fn do_select(&mut self) -> Result<()> {
        // Clear per-cycle state.
        self.exclusion_set.clear();
        self.retry_count = 0;
        self.current_bead = None;

        self.health.update_heartbeat(None).await?;

        let candidate_id = self.strands.select(self.store.as_ref()).await?;

        match candidate_id {
            Some(id) => {
                tracing::debug!(bead_id = %id, "candidate found");
                // Store the candidate ID temporarily — we need it for claiming.
                // We'll set current_bead after successful claim.
                self.current_bead = Some(Bead {
                    id,
                    title: String::new(),
                    body: None,
                    priority: 0,
                    status: crate::types::BeadStatus::Open,
                    assignee: None,
                    labels: vec![],
                    workspace: std::path::PathBuf::new(),
                    dependencies: vec![],
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                });
                self.set_state(WorkerState::Claiming)?;
            }
            None => {
                self.set_state(WorkerState::Exhausted)?;
            }
        }

        Ok(())
    }

    /// CLAIMING: attempt to claim the selected bead.
    async fn do_claim(&mut self) -> Result<()> {
        let bead_id = match self.current_bead {
            Some(ref b) => b.id.clone(),
            None => {
                self.set_state(WorkerState::Selecting)?;
                return Ok(());
            }
        };

        let claim = self.claimer.claim_one(&bead_id, &self.worker_name).await?;

        match claim {
            ClaimResult::Claimed(bead) => {
                tracing::info!(bead_id = %bead.id, title = %bead.title, "claimed bead");
                self.current_bead = Some(bead);
                self.set_state(WorkerState::Building)?;
            }
            ClaimResult::RaceLost { claimed_by } => {
                tracing::debug!(bead_id = %bead_id, %claimed_by, "claim race lost");
                self.exclusion_set.insert(bead_id);
                self.retry_count += 1;
                self.set_state(WorkerState::Retrying)?;
            }
            ClaimResult::NotClaimable { reason } => {
                tracing::debug!(bead_id = %bead_id, %reason, "bead not claimable");
                self.exclusion_set.insert(bead_id);
                self.current_bead = None;
                self.set_state(WorkerState::Selecting)?;
            }
        }

        Ok(())
    }

    /// RETRYING: decide whether to retry claiming or move on.
    fn do_retry(&mut self) -> Result<()> {
        if self.retry_count < self.config.worker.max_claim_retries {
            // Try the next candidate from the same strand cycle.
            self.set_state(WorkerState::Selecting)?;
        } else {
            tracing::debug!(
                retry_count = self.retry_count,
                "max claim retries exceeded, moving to next strand cycle"
            );
            self.retry_count = 0;
            self.exclusion_set.clear();
            self.current_bead = None;
            self.set_state(WorkerState::Selecting)?;
        }
        Ok(())
    }

    /// BUILDING: construct prompt from claimed bead.
    fn do_build(&mut self) -> Result<()> {
        let bead = match self.current_bead {
            Some(ref b) => b,
            None => {
                bail!("BUILDING state without current_bead — invariant violated");
            }
        };

        let prompt = self.prompt_builder.build_pluck(
            bead,
            &self.config.workspace.default,
            &self.worker_name,
        )?;

        // Store the prompt for the dispatch phase. We use a transient field pattern:
        // the prompt is passed via self.built_prompt.
        self.built_prompt = Some(prompt);
        self.set_state(WorkerState::Dispatching)?;
        Ok(())
    }

    /// DISPATCHING: resolve adapter and prepare for execution.
    async fn do_dispatch(&mut self) -> Result<()> {
        let bead = match self.current_bead {
            Some(ref b) => b,
            None => {
                bail!("DISPATCHING state without current_bead — invariant violated");
            }
        };

        self.health.update_heartbeat(Some(&bead.id)).await?;
        self.set_state(WorkerState::Executing)?;
        Ok(())
    }

    /// EXECUTING: run the agent process and capture output.
    async fn do_execute(&mut self) -> Result<()> {
        let bead = match self.current_bead {
            Some(ref b) => b.clone(),
            None => {
                bail!("EXECUTING state without current_bead — invariant violated");
            }
        };

        let prompt = match self.built_prompt.take() {
            Some(p) => p,
            None => {
                bail!("EXECUTING state without built_prompt — invariant violated");
            }
        };

        let adapter = self.resolve_adapter()?;

        // Race the dispatch against the shutdown signal.
        let was_interrupted;
        let exec_result = if self.shutdown.load(Ordering::SeqCst) {
            // Already shutting down — don't start the agent.
            was_interrupted = true;
            None
        } else {
            let result = self
                .dispatcher
                .dispatch(&bead.id, &prompt, &adapter, &self.config.workspace.default)
                .await?;
            was_interrupted = self.shutdown.load(Ordering::SeqCst);
            Some(result)
        };

        let output = match exec_result {
            Some(result) => AgentOutcome {
                exit_code: result.exit_code,
                stdout: result.stdout,
                stderr: result.stderr,
            },
            None => AgentOutcome {
                exit_code: 130, // Simulated SIGINT
                stdout: String::new(),
                stderr: "interrupted before execution".to_string(),
            },
        };

        self.exec_output = Some((output, was_interrupted));
        self.set_state(WorkerState::Handling)?;
        Ok(())
    }

    /// HANDLING: classify outcome and route to handler.
    async fn do_handle(&mut self) -> Result<()> {
        let bead = match self.current_bead {
            Some(ref b) => b.clone(),
            None => {
                bail!("HANDLING state without current_bead — invariant violated");
            }
        };

        let (output, was_interrupted) = match self.exec_output.take() {
            Some(pair) => pair,
            None => {
                bail!("HANDLING state without exec_output — invariant violated");
            }
        };

        self.outcome_handler
            .handle(self.store.as_ref(), &bead, &output, was_interrupted)
            .await?;

        if was_interrupted {
            // After handling interrupted outcome, stop the worker.
            self.set_state(WorkerState::Stopped)?;
        } else {
            self.set_state(WorkerState::Logging)?;
        }

        Ok(())
    }

    /// LOGGING: record telemetry and prepare for next cycle.
    fn do_log(&mut self) -> Result<()> {
        self.beads_processed += 1;
        self.current_bead = None;
        self.set_state(WorkerState::Selecting)?;
        Ok(())
    }

    // ── Terminal state handlers ─────────────────────────────────────────────

    /// Handle the EXHAUSTED state: either wait and retry or exit.
    async fn handle_exhausted(&mut self) -> Result<WorkerState> {
        self.telemetry.emit(EventKind::WorkerExhausted {
            cycle_count: self.beads_processed,
            last_strand: self
                .strands
                .strand_names()
                .last()
                .unwrap_or(&"none")
                .to_string(),
        })?;

        match self.config.worker.idle_action {
            IdleAction::Wait => {
                let backoff = self.config.worker.idle_timeout;
                tracing::info!(
                    backoff_secs = backoff,
                    "all strands exhausted, waiting before retry"
                );
                self.telemetry.emit(EventKind::WorkerIdle {
                    backoff_seconds: backoff,
                })?;
                tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                self.state = WorkerState::Selecting;
                Ok(WorkerState::Selecting)
            }
            IdleAction::Exit => {
                tracing::info!("all strands exhausted and idle_action=exit, stopping");
                self.stop("exhausted").await
            }
        }
    }

    /// Graceful stop: emit telemetry and return terminal state.
    async fn stop(&mut self, reason: &str) -> Result<WorkerState> {
        let uptime = self.boot_time.map(|t| t.elapsed().as_secs()).unwrap_or(0);

        self.telemetry.emit(EventKind::WorkerStopped {
            reason: reason.to_string(),
            beads_processed: self.beads_processed,
            uptime_secs: uptime,
        })?;

        tracing::info!(
            reason,
            beads_processed = self.beads_processed,
            uptime_secs = uptime,
            "worker stopped"
        );

        Ok(WorkerState::Stopped)
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    /// Transition to a new state, emitting telemetry.
    fn set_state(&mut self, to: WorkerState) -> Result<()> {
        let from = self.state.clone();
        tracing::debug!(from = %from, to = %to, "state transition");
        self.telemetry.emit(EventKind::StateTransition {
            from,
            to: to.clone(),
        })?;
        self.state = to;
        Ok(())
    }

    /// Resolve the agent adapter from config, with fallback to built-in.
    fn resolve_adapter(&self) -> Result<crate::dispatch::AgentAdapter> {
        let adapter_name = &self.config.agent.default;

        if let Some(adapter) = self.dispatcher.adapter(adapter_name) {
            return Ok(adapter.clone());
        }

        // Fall back to claude-sonnet built-in.
        if let Some(adapter) = self.dispatcher.adapter("claude-sonnet") {
            tracing::warn!(
                requested = %adapter_name,
                fallback = "claude-sonnet",
                "configured adapter not found, using fallback"
            );
            return Ok(adapter.clone());
        }

        // Last resort: first built-in adapter.
        match crate::dispatch::builtin_adapters().into_iter().next() {
            Some(adapter) => {
                tracing::warn!("no adapters available, using first built-in");
                Ok(adapter)
            }
            None => bail!("no agent adapters available"),
        }
    }

    /// Return the current worker state (for testing/inspection).
    pub fn state(&self) -> &WorkerState {
        &self.state
    }

    /// Return the number of beads processed so far.
    pub fn beads_processed(&self) -> u64 {
        self.beads_processed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::{BeadStore, Filters, RepairReport};
    use crate::types::{Bead, BeadId, BeadStatus, ClaimResult};
    use async_trait::async_trait;
    use std::sync::Mutex;

    // ── Mock BeadStore ──

    struct MockStore {
        beads: Mutex<Vec<Bead>>,
    }

    impl MockStore {
        fn new(beads: Vec<Bead>) -> Self {
            MockStore {
                beads: Mutex::new(beads),
            }
        }

        fn empty() -> Self {
            Self::new(vec![])
        }
    }

    #[async_trait]
    impl BeadStore for MockStore {
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().unwrap().clone())
        }
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(self.beads.lock().unwrap().clone())
        }
        async fn show(&self, id: &BeadId) -> Result<Bead> {
            self.beads
                .lock()
                .unwrap()
                .iter()
                .find(|b| b.id == *id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))
        }
        async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
            let mut beads = self.beads.lock().unwrap();
            if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
                bead.status = BeadStatus::InProgress;
                bead.assignee = Some(actor.to_string());
                Ok(ClaimResult::Claimed(bead.clone()))
            } else {
                anyhow::bail!("bead not found: {id}")
            }
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
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
        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("new-bead"))
        }
        async fn doctor_repair(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
    }

    fn make_test_bead(id: &str) -> Bead {
        Bead {
            id: BeadId::from(id),
            title: format!("Test bead {id}"),
            body: Some("Do the thing".to_string()),
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: vec![],
            workspace: std::path::PathBuf::from("/tmp/test-workspace"),
            dependencies: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn make_worker(store: Arc<dyn BeadStore>) -> Worker {
        let config = Config::default();
        Worker::new(config, "test-worker".to_string(), store)
    }

    #[tokio::test]
    async fn worker_starts_in_booting_state() {
        let store = Arc::new(MockStore::empty());
        let worker = make_worker(store);
        assert_eq!(*worker.state(), WorkerState::Booting);
    }

    #[tokio::test]
    async fn boot_validates_config() {
        let store = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.agent.default = String::new(); // Invalid
        let mut worker = Worker::new(config, "test-worker".to_string(), store);
        let result = worker.boot();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("agent.default"));
    }

    #[tokio::test]
    async fn boot_transitions_to_selecting() {
        let store = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();
        assert_eq!(*worker.state(), WorkerState::Selecting);
    }

    #[tokio::test]
    async fn run_with_empty_store_returns_exhausted_or_stopped() {
        let store = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.worker.idle_action = IdleAction::Exit;
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        let result = worker.run().await.unwrap();
        assert!(
            result == WorkerState::Stopped || result == WorkerState::Exhausted,
            "expected Stopped or Exhausted, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn resolve_adapter_returns_builtin() {
        let store = Arc::new(MockStore::empty());
        let worker = make_worker(store);
        let adapter = worker.resolve_adapter().unwrap();
        // Default config uses "claude" which won't match "claude-sonnet" directly,
        // but the fallback chain should find a built-in.
        assert!(!adapter.name.is_empty());
    }

    #[tokio::test]
    async fn beads_processed_starts_at_zero() {
        let store = Arc::new(MockStore::empty());
        let worker = make_worker(store);
        assert_eq!(worker.beads_processed(), 0);
    }

    #[tokio::test]
    async fn do_select_with_no_beads_transitions_to_exhausted() {
        let store = Arc::new(MockStore::empty());
        let mut worker = make_worker(store);
        worker.boot().unwrap();

        worker.do_select().await.unwrap();
        assert_eq!(*worker.state(), WorkerState::Exhausted);
    }

    #[tokio::test]
    async fn shutdown_flag_causes_stop() {
        let store = Arc::new(MockStore::empty());
        let mut config = Config::default();
        config.worker.idle_action = IdleAction::Exit;
        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Set shutdown before run.
        worker.shutdown.store(true, Ordering::SeqCst);

        let result = worker.run().await.unwrap();
        assert_eq!(result, WorkerState::Stopped);
    }

    #[tokio::test]
    async fn do_select_with_beads_transitions_to_claiming() {
        let bead = make_test_bead("needle-test-001");
        let store = Arc::new(MockStore::new(vec![bead]));
        let mut worker = make_worker(store);
        worker.boot().unwrap();

        worker.do_select().await.unwrap();
        assert_eq!(*worker.state(), WorkerState::Claiming);
        assert!(worker.current_bead.is_some());
    }

    #[tokio::test]
    async fn full_cycle_with_echo_agent() {
        use std::collections::HashMap;

        // Test a full cycle: select → claim → build → dispatch → execute → handle → log
        let bead = make_test_bead("needle-echo");
        let store: Arc<dyn BeadStore> = Arc::new(MockStore::new(vec![bead]));
        let mut config = Config::default();
        config.worker.idle_action = IdleAction::Exit;
        // Use a simple echo adapter so the test finishes quickly.
        config.agent.default = "echo-test".to_string();
        config.agent.timeout = 5;

        let mut worker = Worker::new(config, "test-worker".to_string(), store);

        // Replace the dispatcher with one that has a simple echo adapter.
        let echo_adapter = crate::dispatch::AgentAdapter {
            name: "echo-test".to_string(),
            description: None,
            agent_cli: "echo".to_string(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: "echo done".to_string(),
            environment: HashMap::new(),
            timeout_secs: 5,
            provider: None,
            model: None,
        };
        let mut adapters = HashMap::new();
        adapters.insert("echo-test".to_string(), echo_adapter);
        worker.dispatcher =
            Dispatcher::with_adapters(adapters, Telemetry::new("test-worker".to_string()), 5);

        let result = worker.run().await.unwrap();
        assert!(
            result == WorkerState::Stopped || result == WorkerState::Exhausted,
            "expected terminal state, got {:?}",
            result
        );
        // At least one bead was processed through the pipeline.
        assert!(
            worker.beads_processed() >= 1,
            "expected at least 1 bead processed, got {}",
            worker.beads_processed()
        );
    }
}
