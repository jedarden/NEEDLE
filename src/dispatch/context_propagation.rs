//! Focused unit coverage for carrying the resolved target-workspace store
//! context from selection and claim into dispatch (needle-d0f287f9).
//!
//! A worker resolves a target workspace, opens its store, claims through
//! that store, and hands the resulting [`DispatchContext`] to
//! [`Dispatcher::dispatch_with_context`]. These tests pin the propagation
//! contract the worker relies on:
//!
//! - [`ResolvedStoreContext`] hands back the exact store handle it was
//!   bound to — the handle is carried, never rebuilt from a workspace path.
//! - The final pre-spawn claim gate consults only the store carried in the
//!   context. The dispatcher's wired (home) store is not queried for a
//!   context-carrying dispatch, and a mismatch is attributed to the
//!   context's workspace, not the dispatch call's workspace argument.
//! - Local-workspace routing stays supported: the home store bound as the
//!   carried context (the worker's local-bead capture) dispatches
//!   unchanged.
//! - The context-free compatibility path stays fail-closed when no verifier
//!   is wired — it never substitutes a store of its own finding.
//!
//! Process-level coverage of the same contract (worker → dispatcher →
//! child process) lives in `tests/p2_integration_tests/fail_closed_verification.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Result};
use async_trait::async_trait;

use super::{
    builtin_adapters, is_claim_verification_error, AgentAdapter, DispatchContext, Dispatcher,
};
use crate::bead_store::{BeadStore, Filters, RepairReport};
use crate::claim::{ClaimIdentity, ResolvedStoreContext};
use crate::prompt::BuiltPrompt;
use crate::telemetry::test_utils::MemorySink;
use crate::telemetry::{Telemetry, TelemetryEvent};
use crate::types::{Bead, BeadId, BeadStatus, ClaimResult, ClaimStatus, InputMethod};

/// Actor both the claim identity and the probe stores agree on.
const CONTEXT_WORKER: &str = "context-propagation-worker";
/// Name the probe adapter is registered under.
const PROBE_ADAPTER: &str = "context-probe";
/// File the probe command leaves behind so a test can prove a child
/// process really spawned (and, on aborts, really did not).
const SPAWN_SENTINEL: &str = "spawned.txt";
/// Workspace label bound into the carried context in the roaming test —
/// deliberately not a real directory: nothing may read it.
const CARRIED_WORKSPACE: &str = "/selected/target-workspace";
/// Revision the claim identity captured when the claim landed.
const CAPTURED_REVISION: u64 = 7;

/// Minimal bead store for the pre-spawn gate: answers every `claim_status`
/// query from one fixed status and counts how often it was consulted. No
/// other operation is reachable in these tests, so they refuse rather than
/// pretend to succeed.
struct ProbeStore {
    status: ClaimStatus,
    claim_queries: AtomicUsize,
}

impl ProbeStore {
    /// A store that reports `actor` still holding a live claim at the given
    /// revision and claim epoch.
    fn claimed_by(actor: &str, revision: u64, claim_epoch: u64) -> Self {
        ProbeStore {
            status: ClaimStatus {
                status: BeadStatus::InProgress,
                assignee: Some(actor.to_string()),
                revision: Some(revision),
                claim_epoch: Some(claim_epoch),
            },
            claim_queries: AtomicUsize::new(0),
        }
    }

    /// How many times the pre-spawn gate queried this store.
    fn claim_queries(&self) -> usize {
        self.claim_queries.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl BeadStore for ProbeStore {
    async fn claim_status(&self, _id: &BeadId) -> Result<ClaimStatus> {
        self.claim_queries.fetch_add(1, Ordering::SeqCst);
        Ok(self.status.clone())
    }

    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        Ok(Vec::new())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(Vec::new())
    }

    async fn show(&self, _id: &BeadId) -> Result<Bead> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn release(&self, _id: &BeadId) -> Result<()> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn block(&self, _id: &BeadId) -> Result<()> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> Result<()> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn add_dependency(&self, _blocker: &BeadId, _blocked: &BeadId) -> Result<()> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn remove_dependency(&self, _blocked: &BeadId, _blocker: &BeadId) -> Result<()> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        bail!("ProbeStore serves claim_status only")
    }

    async fn full_rebuild(&self) -> Result<()> {
        bail!("ProbeStore serves claim_status only")
    }

    fn has_valid_store(&self) -> bool {
        false
    }
}

/// Adapter whose command leaves a sentinel file in the dispatch workspace.
/// Built by mutating a builtin so the test does not enumerate
/// `AgentAdapter` fields — additions to that struct must not break these
/// tests.
fn probe_adapter() -> AgentAdapter {
    let mut adapter = builtin_adapters()
        .into_iter()
        .next()
        .expect("builtin adapters are never empty");
    adapter.name = PROBE_ADAPTER.to_string();
    adapter.description = None;
    adapter.agent_cli = "bash".to_string();
    adapter.version_command = None;
    adapter.input_method = InputMethod::Stdin;
    adapter.invoke_template = format!("touch {{workspace}}/{SPAWN_SENTINEL}");
    adapter.environment = HashMap::new();
    adapter.timeout_secs = 30;
    adapter.idle_timeout_secs = 0;
    adapter.hard_timeout_secs = 30;
    adapter.provider = None;
    adapter.model = None;
    adapter.usage_format = None;
    adapter.output_transform = None;
    adapter
}

/// The prompt handed to `dispatch_with_context`; only its content matters.
fn probe_prompt() -> BuiltPrompt {
    BuiltPrompt {
        content: "context propagation probe".to_string(),
        hash: String::new(),
        token_estimate: 7,
        template_name: "context-propagation".to_string(),
        template_version: "test".to_string(),
    }
}

/// The identity captured when the claim landed in the target store.
fn captured_identity() -> ClaimIdentity {
    ClaimIdentity {
        actor: CONTEXT_WORKER.to_string(),
        revision: Some(CAPTURED_REVISION),
        claim_epoch: Some(2),
    }
}

/// Data payloads of every captured event of one type, in emission order.
fn events_of_type(events: &Mutex<Vec<TelemetryEvent>>, event_type: &str) -> Vec<serde_json::Value> {
    events
        .lock()
        .expect("telemetry event lock poisoned")
        .iter()
        .filter(|event| event.event_type == event_type)
        .map(|event| event.data.clone())
        .collect()
}

/// Give the memory sink's writer task time to drain after the dispatcher
/// that owned the telemetry emitter is dropped (the claim module's tests
/// use the same pattern).
async fn drain_telemetry() {
    tokio::time::sleep(Duration::from_millis(200)).await;
}

/// The resolved context is an owned handle: reading it back yields the very
/// store that was bound — never a store rebuilt from the workspace path or
/// swapped for the worker's home store.
#[test]
fn resolved_store_context_carries_the_exact_bound_store() {
    let store: Arc<dyn BeadStore> =
        Arc::new(ProbeStore::claimed_by(CONTEXT_WORKER, CAPTURED_REVISION, 2));
    let context = ResolvedStoreContext::new(Arc::clone(&store), PathBuf::from(CARRIED_WORKSPACE));

    assert!(
        Arc::ptr_eq(&context.store(), &store),
        "store() must hand back the exact store that was bound, not a rebuilt one"
    );
    assert_eq!(context.workspace(), Path::new(CARRIED_WORKSPACE));
}

/// The dispatch context binds the resolved store to the claim identity
/// captured from it, and cloning it — what the worker does when handing it
/// to the dispatcher — preserves the store allocation and every identity
/// field, including the claim epoch used by the final fence.
#[test]
fn dispatch_context_clone_preserves_store_and_claim_identity() {
    let store: Arc<dyn BeadStore> =
        Arc::new(ProbeStore::claimed_by(CONTEXT_WORKER, CAPTURED_REVISION, 2));
    let identity = captured_identity();

    let context = DispatchContext::new(
        ResolvedStoreContext::new(Arc::clone(&store), PathBuf::from(CARRIED_WORKSPACE)),
        identity.clone(),
    );
    let clone = context.clone();

    assert!(
        Arc::ptr_eq(&clone.target_store().store(), &store),
        "cloning the context must keep the same store allocation"
    );
    assert_eq!(clone.claim_identity(), &identity);
    assert_eq!(
        clone.target_store().workspace(),
        Path::new(CARRIED_WORKSPACE)
    );
}

/// The pre-spawn gate must verify through the carried context store and
/// nothing else. The dispatcher's wired home store reports a perfectly
/// matching claim — if the gate consulted it, the dispatch would spawn —
/// while the carried store reports that the claim identity moved after
/// capture (revision bumped). The gate must abort before any child process
/// exists, consult the home store zero times, and attribute the failure to
/// the context's workspace, not the dispatch call's workspace argument.
#[tokio::test]
async fn pre_spawn_gate_verifies_only_the_carried_target_store() {
    let home = Arc::new(ProbeStore::claimed_by(CONTEXT_WORKER, CAPTURED_REVISION, 2));
    let target = Arc::new(ProbeStore::claimed_by(
        CONTEXT_WORKER,
        CAPTURED_REVISION + 1,
        2,
    ));
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink(CONTEXT_WORKER.to_string(), sink);

    let mut adapters = HashMap::new();
    let adapter = probe_adapter();
    adapters.insert(adapter.name.clone(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 60)
        .with_bead_store(home.clone())
        .with_worker_id(CONTEXT_WORKER.to_string());

    let dispatch_workspace = tempfile::tempdir().expect("create dispatch workspace");
    let context = DispatchContext::new(
        ResolvedStoreContext::new(target.clone(), PathBuf::from(CARRIED_WORKSPACE)),
        captured_identity(),
    );

    let result = dispatcher
        .dispatch_with_context(
            &BeadId::from("needle-context-probe-moved"),
            &probe_prompt(),
            dispatcher
                .adapter(PROBE_ADAPTER)
                .expect("probe adapter is registered"),
            dispatch_workspace.path(),
            &context,
        )
        .await;

    let error = result.expect_err("a moved claim identity in the carried store must abort");
    assert!(
        is_claim_verification_error(&error),
        "the abort must surface as a claim-verification error, got: {error:#}"
    );
    assert!(
        !dispatch_workspace.path().join(SPAWN_SENTINEL).exists(),
        "a failed verification must spawn zero child processes"
    );
    assert_eq!(
        target.claim_queries(),
        1,
        "the gate must query the carried store exactly once"
    );
    assert_eq!(
        home.claim_queries(),
        0,
        "the wired home store must never be consulted for a carried-context dispatch"
    );

    drop(dispatcher);
    drain_telemetry().await;

    let recheck_failed = events_of_type(&events, "bead.claim.recheck_failed");
    assert_eq!(recheck_failed.len(), 1, "one mismatch, one record");
    assert_eq!(
        recheck_failed[0]["target_workspace"],
        serde_json::json!(CARRIED_WORKSPACE),
        "verification diagnostics must name the carried context's workspace, \
         not the dispatch call's workspace argument"
    );
    assert!(
        events_of_type(&events, "bead.claim.verify_error").is_empty(),
        "a verification that ran and compared identity must not also emit verify_error"
    );
}

/// Local-workspace routing stays supported: the home store bound as the
/// carried context (the worker's local-bead capture) passes the gate and
/// the child spawns. The dispatcher's wired verifier fields matching the
/// context is exactly the local shape.
#[tokio::test]
async fn local_routing_through_the_carried_home_store_still_dispatches() {
    let home = Arc::new(ProbeStore::claimed_by(CONTEXT_WORKER, CAPTURED_REVISION, 2));
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink(CONTEXT_WORKER.to_string(), sink);

    let mut adapters = HashMap::new();
    let adapter = probe_adapter();
    adapters.insert(adapter.name.clone(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 60)
        .with_bead_store(home.clone())
        .with_worker_id(CONTEXT_WORKER.to_string());

    let workspace = tempfile::tempdir().expect("create local workspace");
    let context = DispatchContext::new(
        ResolvedStoreContext::new(home.clone(), workspace.path().to_path_buf()),
        captured_identity(),
    );

    let execution = dispatcher
        .dispatch_with_context(
            &BeadId::from("needle-context-probe-local"),
            &probe_prompt(),
            dispatcher
                .adapter(PROBE_ADAPTER)
                .expect("probe adapter is registered"),
            workspace.path(),
            &context,
        )
        .await
        .expect("a matching carried context must dispatch");

    assert_eq!(execution.exit_code, 0, "probe command should succeed");
    assert!(
        workspace.path().join(SPAWN_SENTINEL).exists(),
        "the control case must actually spawn — otherwise the sentinel is broken"
    );
    assert_eq!(home.claim_queries(), 1);

    drop(dispatcher);
    drain_telemetry().await;

    assert_eq!(
        events_of_type(&events, "bead.claim.recheck_succeeded").len(),
        1,
        "a passed pre-spawn verification records exactly one recheck_succeeded"
    );
    assert!(events_of_type(&events, "bead.claim.verify_error").is_empty());
}

/// Without a carried context the only remaining path is the legacy
/// local-workspace one — and it must stay fail-closed when its verifier
/// fields are absent, never reconstructing a store from the workspace path
/// to fill the gap.
#[tokio::test]
async fn context_free_dispatch_without_a_wired_verifier_fails_closed() {
    let (sink, events) = MemorySink::new();
    let telemetry = Telemetry::with_sink(CONTEXT_WORKER.to_string(), sink);

    let mut adapters = HashMap::new();
    let adapter = probe_adapter();
    adapters.insert(adapter.name.clone(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 60);

    let workspace = tempfile::tempdir().expect("create legacy workspace");

    let result = dispatcher
        .dispatch(
            &BeadId::from("needle-context-probe-legacy"),
            &probe_prompt(),
            dispatcher
                .adapter(PROBE_ADAPTER)
                .expect("probe adapter is registered"),
            workspace.path(),
        )
        .await;

    let error = result.expect_err("no wired verifier and no context must abort");
    assert!(
        is_claim_verification_error(&error),
        "the abort must surface as a claim-verification error, got: {error:#}"
    );
    assert!(
        error
            .to_string()
            .contains("no bead store wired for pre-spawn claim verification"),
        "the error must name the missing verifier, got: {error:#}"
    );
    assert!(
        !workspace.path().join(SPAWN_SENTINEL).exists(),
        "a fail-closed path must spawn zero child processes"
    );

    drop(dispatcher);
    drain_telemetry().await;

    let verify_error = events_of_type(&events, "bead.claim.verify_error");
    assert_eq!(verify_error.len(), 1);
    assert_eq!(verify_error[0]["stage"], serde_json::json!("pre_spawn"));
}
