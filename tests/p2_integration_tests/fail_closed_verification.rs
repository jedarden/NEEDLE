//! Fail-closed claim verification at the pre-spawn gate.
//!
//! The dispatcher's pre-spawn re-check is the last gate before a child
//! process is created. These tests inject every verification failure class —
//! issue not found, wrong backend identity, malformed JSON response, timeout,
//! unavailable CLI, and an unwired verifier — and assert that in each case
//! dispatch aborts with ZERO spawned child processes. A spawn attempt is
//! observable through a sentinel file: the adapter's invoke template appends
//! to it, so any spawn leaves a trace. A control case proves the sentinel
//! actually detects a spawn.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tempfile::TempDir;

use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::claim::{ClaimIdentity, ResolvedStoreContext};
use needle::dispatch::{AgentAdapter, DispatchContext, Dispatcher, TokenExtraction};
use needle::prompt::BuiltPrompt;
use needle::telemetry::Telemetry;
use needle::types::{Bead, BeadId, BeadStatus, ClaimResult, ClaimStatus};

const WORKER_ID: &str = "probe-worker";

/// What the probe store answers when the pre-spawn check queries it.
enum ProbeOutcome {
    /// The store answered with this claim status.
    Live(ClaimStatus),
    /// The store could not answer (lookup, parse, timeout, CLI failure…).
    StoreError(String),
}

/// Bead store whose `claim_status` serves an injected outcome.
struct ProbeStore {
    outcome: Mutex<ProbeOutcome>,
}

impl ProbeStore {
    fn new(outcome: ProbeOutcome) -> Self {
        ProbeStore {
            outcome: Mutex::new(outcome),
        }
    }
}

#[async_trait]
impl BeadStore for ProbeStore {
    async fn ready(&self, _filters: &Filters) -> anyhow::Result<Vec<Bead>> {
        Ok(vec![])
    }

    async fn list_all(&self) -> anyhow::Result<Vec<Bead>> {
        Ok(vec![])
    }

    async fn show(&self, _id: &BeadId) -> anyhow::Result<Bead> {
        anyhow::bail!("ProbeStore serves claim_status only")
    }

    async fn claim_status(&self, _id: &BeadId) -> anyhow::Result<ClaimStatus> {
        match &*self.outcome.lock().unwrap() {
            ProbeOutcome::Live(status) => Ok(status.clone()),
            ProbeOutcome::StoreError(message) => Err(anyhow::anyhow!(message.clone())),
        }
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> anyhow::Result<ClaimResult> {
        anyhow::bail!("ProbeStore serves claim_status only")
    }

    async fn claim_auto(&self, _actor: &str) -> anyhow::Result<ClaimResult> {
        anyhow::bail!("ProbeStore serves claim_status only")
    }

    async fn release(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn block(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn create_bead(
        &self,
        _title: &str,
        _body: &str,
        _labels: &[&str],
    ) -> anyhow::Result<BeadId> {
        anyhow::bail!("ProbeStore serves claim_status only")
    }

    async fn add_dependency(
        &self,
        _blocker_id: &BeadId,
        _blocked_id: &BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_dependency(
        &self,
        _blocked_id: &BeadId,
        _blocker_id: &BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn doctor_repair(&self) -> anyhow::Result<RepairReport> {
        Ok(RepairReport {
            warnings: Vec::new(),
            fixed: Vec::new(),
        })
    }

    async fn doctor_check(&self) -> anyhow::Result<RepairReport> {
        Ok(RepairReport {
            warnings: Vec::new(),
            fixed: Vec::new(),
        })
    }

    async fn full_rebuild(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

/// In-memory telemetry capture: the dispatcher's telemetry writes JSONL into
/// a temp log dir, which the assertions read back.
struct TelemetryLog {
    dir: TempDir,
}

impl TelemetryLog {
    fn new() -> Self {
        TelemetryLog {
            dir: TempDir::new().expect("create telemetry log dir"),
        }
    }

    fn telemetry(&self) -> Telemetry {
        let telemetry = Telemetry::with_log_dir(WORKER_ID.to_string(), self.dir.path());
        telemetry.start();
        telemetry
    }

    fn events(&self) -> Vec<serde_json::Value> {
        let mut events = Vec::new();
        let entries = match std::fs::read_dir(self.dir.path()) {
            Ok(entries) => entries,
            Err(error) => panic!("read telemetry log dir: {error}"),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            for line in content.lines().filter(|line| !line.trim().is_empty()) {
                match serde_json::from_str::<serde_json::Value>(line) {
                    Ok(event) => events.push(event),
                    Err(error) => panic!("parse telemetry line from {path:?}: {error}"),
                }
            }
        }
        events
    }

    fn count(&self, event_type: &str) -> usize {
        self.events()
            .iter()
            .filter(|event| event["event_type"] == event_type)
            .count()
    }

    fn verify_error_category(&self) -> Option<String> {
        self.events()
            .iter()
            .find(|event| event["event_type"] == "bead.claim.verify_error")
            .and_then(|event| event["data"]["category"].as_str().map(String::from))
    }
}

fn probe_adapter() -> AgentAdapter {
    AgentAdapter {
        name: "fail-closed-probe".to_string(),
        description: None,
        agent_cli: "test".to_string(),
        version_command: None,
        // This probe does not consume the prompt. Using args input keeps the
        // spawn sentinel independent of the shared prompt-file lifecycle when
        // the five gate cases run concurrently.
        input_method: needle::types::InputMethod::Args {
            flag: "--prompt".to_string(),
        },
        // Any spawn appends to the sentinel file in the workspace — the
        // observable a zero-spawn assertion reads.
        invoke_template: "printf '%s:%s\\n' \"${NEEDLE_BEAD_REVISION:-missing}\" \"${NEEDLE_BEAD_FENCING_TOKEN:-missing}\" >> {workspace}/claim-context.txt; echo spawned >> {workspace}/spawned.txt".to_string(),
        environment: HashMap::new(),
        timeout_secs: 0,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        usage_format: None,
        output_transform: None,
        harness: None,
        harness_version: None,
    }
}

fn probe_prompt() -> BuiltPrompt {
    BuiltPrompt {
        content: "fail-closed probe".to_string(),
        hash: "probehash".to_string(),
        token_estimate: 4,
        template_name: "probe".to_string(),
        template_version: "probe-1".to_string(),
    }
}

fn claimed_status(assignee: &str) -> ClaimStatus {
    ClaimStatus {
        status: BeadStatus::InProgress,
        assignee: Some(assignee.to_string()),
        revision: Some(7),
        claim_epoch: Some(2),
    }
}

/// Outcome of one dispatch through the pre-spawn gate.
struct GateOutcome {
    workspace: TempDir,
    telemetry: TelemetryLog,
    dispatch: anyhow::Result<needle::dispatch::ExecutionResult>,
}

impl GateOutcome {
    fn spawned(&self) -> bool {
        self.workspace.path().join("spawned.txt").exists()
    }

    fn count(&self, event_type: &str) -> usize {
        self.telemetry.count(event_type)
    }

    fn verify_error_category(&self) -> Option<String> {
        self.telemetry.verify_error_category()
    }
}

/// Drive one dispatch through the pre-spawn gate with the injected outcome.
async fn run_gate(outcome: ProbeOutcome, wire_store: bool, wire_worker: bool) -> GateOutcome {
    let workspace = TempDir::new().expect("create probe workspace");
    let telemetry_log = TelemetryLog::new();
    let telemetry = telemetry_log.telemetry();

    let mut adapters = HashMap::new();
    let adapter = probe_adapter();
    adapters.insert(adapter.name.clone(), adapter);

    let mut dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    if wire_store {
        dispatcher.set_bead_store(Arc::new(ProbeStore::new(outcome)));
    }
    if wire_worker {
        dispatcher = dispatcher.with_worker_id(WORKER_ID.to_string());
    }

    let bead_id = BeadId::from("needle-fail-closed-probe");
    let dispatch = dispatcher
        .dispatch(
            &bead_id,
            &probe_prompt(),
            dispatcher.adapter("fail-closed-probe").unwrap(),
            workspace.path(),
        )
        .await;

    drop(dispatcher);
    // Give the telemetry writer thread time to drain the channel to disk.
    tokio::time::sleep(Duration::from_millis(200)).await;

    GateOutcome {
        workspace,
        telemetry: telemetry_log,
        dispatch,
    }
}

/// Drive the explicit worker dispatch context through the final pre-spawn
/// gate while deliberately wiring a colliding home store into the dispatcher.
/// A home-store fallback would reject this dispatch; only the selected target
/// store carries the matching claim identity.
async fn run_context_gate(home: ProbeOutcome, target: ProbeOutcome) -> GateOutcome {
    let workspace = TempDir::new().expect("create probe workspace");
    let telemetry_log = TelemetryLog::new();
    let telemetry = telemetry_log.telemetry();

    let mut adapters = HashMap::new();
    let adapter = probe_adapter();
    adapters.insert(adapter.name.clone(), adapter);

    let target_store = Arc::new(ProbeStore::new(target));
    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600)
        .with_bead_store(Arc::new(ProbeStore::new(home)))
        .with_worker_id(WORKER_ID.to_string());
    let context = DispatchContext::new(
        ResolvedStoreContext::new(target_store, workspace.path().to_path_buf()),
        ClaimIdentity {
            actor: WORKER_ID.to_string(),
            revision: Some(7),
            claim_epoch: Some(2),
        },
    );

    let bead_id = BeadId::from("needle-fail-closed-context");
    let dispatch = dispatcher
        .dispatch_with_context(
            &bead_id,
            &probe_prompt(),
            dispatcher.adapter("fail-closed-probe").unwrap(),
            workspace.path(),
            &context,
        )
        .await;

    drop(dispatcher);
    tokio::time::sleep(Duration::from_millis(200)).await;

    GateOutcome {
        workspace,
        telemetry: telemetry_log,
        dispatch,
    }
}

/// Every injected store failure must abort before child process creation,
/// with exactly one verify_error emission naming the failure category.
#[tokio::test]
async fn pre_spawn_store_failure_aborts_before_spawn() {
    let cases = [
        (
            "issue not found",
            "bead not found: needle-fail-closed-probe",
            "lookup",
        ),
        (
            "malformed JSON response",
            "expected value at line 1 column 1",
            "parse",
        ),
        (
            "store query timeout",
            "backend 'bead' operation 'show' timed out after 30s",
            "timeout",
        ),
        (
            "unavailable CLI",
            "backend 'bead' operation 'show' failed using /usr/local/bin/bead: \
             No such file or directory (os error 2)",
            "capability",
        ),
        (
            "unclassified backend error",
            "store shuffled its indexes inexplicably",
            "backend",
        ),
    ];

    for (label, message, expected_category) in cases {
        let outcome = run_gate(ProbeOutcome::StoreError(message.to_string()), true, true).await;

        assert!(
            outcome.dispatch.is_err(),
            "{label}: dispatch must fail when claim verification cannot complete"
        );
        assert!(
            !outcome.spawned(),
            "{label}: a failed verification must spawn zero child processes"
        );
        assert_eq!(
            outcome.count("bead.claim.verify_error"),
            1,
            "{label}: exactly one verify_error emission per failed verification"
        );
        assert_eq!(
            outcome.count("bead.claim.recheck_succeeded"),
            0,
            "{label}: a failed verification must not record a passed re-check"
        );
        assert_eq!(
            outcome.verify_error_category().as_deref(),
            Some(expected_category),
            "{label}: telemetry must name the failure category"
        );
    }
}

/// A wrong backend identity (the live claim belongs to another worker) is a
/// failed precondition: abort before spawn, and record the mismatch as
/// recheck_failed — not as verify_error, which is for verifications that
/// could not complete.
#[tokio::test]
async fn pre_spawn_wrong_identity_aborts_before_spawn() {
    let outcome = run_gate(
        ProbeOutcome::Live(claimed_status("someone-else")),
        true,
        true,
    )
    .await;

    assert!(
        outcome.dispatch.is_err(),
        "dispatch must fail when the live claim belongs to another worker"
    );
    assert!(
        !outcome.spawned(),
        "an identity mismatch must spawn zero child processes"
    );
    assert_eq!(
        outcome.count("bead.claim.recheck_failed"),
        1,
        "the identity mismatch is recorded once as recheck_failed"
    );
    assert_eq!(
        outcome.count("bead.claim.verify_error"),
        0,
        "a verification that ran and compared identity must not also emit verify_error"
    );
}

/// An unwired verifier is a failed precondition, not a bypass: the previous
/// behavior silently skipped verification (and spawned) whenever no bead
/// store was configured on the dispatcher.
#[tokio::test]
async fn pre_spawn_without_store_aborts_before_spawn() {
    let outcome = run_gate(ProbeOutcome::Live(claimed_status(WORKER_ID)), false, true).await;

    assert!(
        outcome.dispatch.is_err(),
        "dispatch must fail closed when no bead store is wired for verification"
    );
    assert!(
        !outcome.spawned(),
        "an unwired verifier must spawn zero child processes"
    );
    assert_eq!(outcome.count("bead.claim.verify_error"), 1);
    assert_eq!(
        outcome.verify_error_category().as_deref(),
        Some("capability"),
        "an unwired verifier is a capability failure"
    );
}

/// Without a worker identity the claim cannot be compared to anything:
/// fail closed rather than spawn unverified.
#[tokio::test]
async fn pre_spawn_without_worker_identity_aborts_before_spawn() {
    let outcome = run_gate(ProbeOutcome::Live(claimed_status(WORKER_ID)), true, false).await;

    assert!(
        outcome.dispatch.is_err(),
        "dispatch must fail closed when no worker identity is wired for verification"
    );
    assert!(
        !outcome.spawned(),
        "a missing worker identity must spawn zero child processes"
    );
    assert_eq!(outcome.count("bead.claim.verify_error"), 1);
    assert_eq!(
        outcome.verify_error_category().as_deref(),
        Some("identity"),
        "a missing worker identity is an identity failure"
    );
}

/// Control: with a live matching claim the gate passes and the spawn happens,
/// proving the sentinel actually detects spawns (the zero-spawn assertions
/// above are not passing vacuously).
#[tokio::test]
async fn pre_spawn_control_with_matching_claim_spawns() {
    let outcome = run_gate(ProbeOutcome::Live(claimed_status(WORKER_ID)), true, true).await;

    let result = outcome
        .dispatch
        .as_ref()
        .expect("a matching live claim must dispatch");
    assert_eq!(result.exit_code, 0, "probe command should succeed");
    assert!(
        outcome.spawned(),
        "the control case must actually spawn — otherwise the sentinel is broken"
    );
    assert_eq!(
        outcome.count("bead.claim.recheck_succeeded"),
        1,
        "a passed pre-spawn verification records exactly one recheck_succeeded"
    );
    assert_eq!(outcome.count("bead.claim.verify_error"), 0);

    assert_context_uses_selected_target_store_and_claim_identity().await;
    assert_context_rejects_moved_claim_identity_from_target_store().await;

    // Keep the real-binary failure fixtures under the fail-closed acceptance
    // filter without adding another harness test budget entry.
    super::dispatch_claim_verification_e2e::assert_failure_matrix_spawns_zero_agents();
}

/// Selection and claim hand the exact remote store plus the claim-time
/// revision/epoch to dispatch. The dispatcher is intentionally configured with
/// a colliding home store that would fail verification if it were consulted.
async fn assert_context_uses_selected_target_store_and_claim_identity() {
    let outcome = run_context_gate(
        ProbeOutcome::Live(ClaimStatus {
            status: BeadStatus::InProgress,
            assignee: Some("home-collision".to_string()),
            revision: Some(99),
            claim_epoch: Some(99),
        }),
        ProbeOutcome::Live(claimed_status(WORKER_ID)),
    )
    .await;

    let result = outcome
        .dispatch
        .as_ref()
        .expect("the selected target store claim must pass verification");
    assert_eq!(result.exit_code, 0);
    assert!(outcome.spawned(), "the target-store dispatch must spawn");
    assert_eq!(
        std::fs::read_to_string(outcome.workspace.path().join("claim-context.txt"))
            .expect("probe should record claim context")
            .trim(),
        "7:2",
        "the claim-time revision and fencing epoch must reach the child"
    );
    assert_eq!(outcome.count("bead.claim.recheck_succeeded"), 1);
}

/// A target store that still names this worker is not enough when its revision
/// or claim epoch moved after claim. The context must make the pre-spawn gate
/// fail closed instead of adopting the newer live credential.
async fn assert_context_rejects_moved_claim_identity_from_target_store() {
    let outcome = run_context_gate(
        ProbeOutcome::Live(claimed_status(WORKER_ID)),
        ProbeOutcome::Live(ClaimStatus {
            status: BeadStatus::InProgress,
            assignee: Some(WORKER_ID.to_string()),
            revision: Some(8),
            claim_epoch: Some(3),
        }),
    )
    .await;

    assert!(outcome.dispatch.is_err());
    assert!(!outcome.spawned());
    assert_eq!(outcome.count("bead.claim.recheck_failed"), 1);
}
