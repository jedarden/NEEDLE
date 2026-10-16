//! Low-water backlog generation coordination.
//!
//! Pluck and Explore run before any generator, so reaching this gate means no
//! fleet candidate survived the normal selection waterfall. The gate performs
//! one final eligible-ready query for the home store, then atomically reserves
//! a workspace-plus-strand generation slot. The durable lease prevents a herd
//! of empty workers from asking the same generator to fill the same plan gap.
//!
//! The gate never blocks a generator outright: when it cannot justify an
//! override it returns [`GeneratorPermit::Ordinary`] and the generator applies
//! its own cooldown policy exactly as it would have without the gate. Only a
//! contended lease suppresses the generator, because that means another worker
//! is already filling the gap this pass would have filled.
//!
//! Generators do not create eligible work: Weave and Pulse file proposals
//! labelled `proposal-pending` + `human`, which the eligible-ready count
//! excludes until an operator admits them. A workspace below the reserve can
//! therefore never climb back above it by generating, and without a bound it
//! received a fresh proposal pass every lease period (needle-b117f398: 4,897
//! proposals in 3.5 days, none admitted). The override is therefore withheld
//! while the workspace already holds `proposal_backlog_cap` or more open,
//! unadmitted generated proposals, and the gate reports `proposal_backlog`.
//! The backlog is counted only after the lease is won, so the full-inventory
//! query runs at most once per workspace and strand per lease period, and the
//! held lease rate-limits the re-check the same way it does a no-gap pass.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bead_store::{BeadStore, Filters};
use crate::config::GenerationConfig;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{BeadId, BeadStatus};

/// Provenance labels of the generators this gate can unleash. A bead carrying
/// one of these plus `proposal-pending` is generated work awaiting admission:
/// Weave files `weave-generated` and Pulse files `pulse-finding`, each beside
/// `proposal-pending` and `human`. Admission removes `proposal-pending`, so an
/// admitted bead stops counting toward the backlog and starts counting as
/// eligible-ready work instead.
const GENERATED_PROPOSAL_LABELS: &[&str] = &["weave-generated", "pulse-finding"];
const PROPOSAL_PENDING_LABEL: &str = "proposal-pending";

/// What a generator may do in the current waterfall pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GeneratorPermit {
    /// Apply the generator's ordinary cooldown policy. Returned when the gate
    /// is disabled, the reserve is zero, the eligible backlog already meets
    /// the reserve, the workspace already holds at least
    /// `proposal_backlog_cap` unadmitted generated proposals, or the gate
    /// could not evaluate the backlog at all.
    Ordinary,
    /// Eligible fleet-ready work is below the reserve and this worker holds
    /// the workspace-plus-strand lease: bypass the cooldown and generate.
    LowWater { fencing_token: String },
    /// Eligible work is below the reserve but another worker holds the lease.
    /// Do not generate; continue the waterfall and let the loser move on.
    Contended,
}

impl GeneratorPermit {
    /// Whether the generator may bypass its ordinary cooldown this pass.
    pub(super) fn bypasses_cooldown(&self) -> bool {
        matches!(self, GeneratorPermit::LowWater { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LeaseRecord {
    owner: String,
    fencing_token: String,
    expires_at_unix: i64,
}

pub(super) struct GeneratorGate {
    config: GenerationConfig,
    workspace: PathBuf,
    state_dir: PathBuf,
    exclude_labels: Vec<String>,
    owner: String,
    telemetry: Telemetry,
}

impl GeneratorGate {
    pub(super) fn new(
        config: GenerationConfig,
        workspace: PathBuf,
        state_dir: PathBuf,
        mut exclude_labels: Vec<String>,
        telemetry: Telemetry,
    ) -> Self {
        // These labels are hard selection policy even when the operator's
        // configurable list is empty. Pluck applies the same defaults; without
        // them the generation gate can call a workspace "healthy" from raw
        // deferred beads immediately before Pluck declares it exhausted.
        for label in ["deferred", "human", "blocked"] {
            if !exclude_labels
                .iter()
                .any(|configured| configured.eq_ignore_ascii_case(label))
            {
                exclude_labels.push(label.to_string());
            }
        }
        Self {
            config,
            workspace,
            state_dir,
            exclude_labels,
            owner: telemetry.worker_id().to_string(),
            telemetry,
        }
    }

    /// Resolve the permit for one generator in one waterfall pass.
    ///
    /// Infallible by design: a gate that cannot count the backlog must not
    /// stop a generator from applying its own policy, so any evaluation error
    /// degrades to [`GeneratorPermit::Ordinary`] after being logged.
    pub(super) async fn prepare(
        &self,
        strand_name: &str,
        store: &dyn BeadStore,
        exclusions: &HashSet<BeadId>,
    ) -> GeneratorPermit {
        if !self.config.enabled || self.config.low_water_reserve == 0 {
            self.emit_gate(strand_name, "gate_disabled", 0);
            return GeneratorPermit::Ordinary;
        }

        let eligible_ready = match self.count_eligible_ready(store, exclusions).await {
            Ok(count) => count,
            Err(error) => {
                tracing::warn!(
                    strand = strand_name,
                    workspace = %self.workspace.display(),
                    error = %error,
                    "low-water gate could not evaluate the backlog; falling back to the generator's ordinary cooldown"
                );
                self.emit_gate(strand_name, "gate_disabled", 0);
                return GeneratorPermit::Ordinary;
            }
        };

        if eligible_ready >= self.config.low_water_reserve {
            self.emit_gate(strand_name, "backlog_healthy", eligible_ready);
            return GeneratorPermit::Ordinary;
        }

        match self.acquire_lease(strand_name) {
            Ok(Some(fencing_token)) => {
                // The lease is deliberately kept when the override is
                // withheld below: it bounds how often any worker re-runs the
                // inventory query for this workspace and strand, exactly as a
                // pass that found no gap would.
                if self.config.proposal_backlog_cap > 0 {
                    match self.count_pending_generated_proposals(store).await {
                        Ok(pending) if pending >= self.config.proposal_backlog_cap => {
                            self.emit_gate(strand_name, "proposal_backlog", eligible_ready);
                            tracing::info!(
                                strand = strand_name,
                                workspace = %self.workspace.display(),
                                eligible_ready,
                                low_water_reserve = self.config.low_water_reserve,
                                pending_generated_proposals = pending,
                                proposal_backlog_cap = self.config.proposal_backlog_cap,
                                "unadmitted generated proposals at or above the cap; withholding the low-water override"
                            );
                            return GeneratorPermit::Ordinary;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(
                                strand = strand_name,
                                workspace = %self.workspace.display(),
                                error = %error,
                                "low-water gate could not count pending generated proposals; falling back to the generator's ordinary cooldown"
                            );
                            self.emit_gate(strand_name, "gate_disabled", eligible_ready);
                            return GeneratorPermit::Ordinary;
                        }
                    }
                }
                self.emit_gate(strand_name, "low_water", eligible_ready);
                tracing::info!(
                    strand = strand_name,
                    workspace = %self.workspace.display(),
                    eligible_ready,
                    low_water_reserve = self.config.low_water_reserve,
                    lease_ttl_secs = self.config.lease_ttl_secs,
                    fencing_token = %fencing_token,
                    "low-water reserve met; overriding generator cooldown"
                );
                GeneratorPermit::LowWater { fencing_token }
            }
            Ok(None) => {
                self.emit_gate(strand_name, "contended", eligible_ready);
                tracing::info!(
                    strand = strand_name,
                    workspace = %self.workspace.display(),
                    eligible_ready,
                    low_water_reserve = self.config.low_water_reserve,
                    "generation lease held elsewhere; continuing the waterfall without generating"
                );
                GeneratorPermit::Contended
            }
            Err(error) => {
                tracing::warn!(
                    strand = strand_name,
                    workspace = %self.workspace.display(),
                    error = %error,
                    "low-water gate could not acquire the generation lease; falling back to the generator's ordinary cooldown"
                );
                self.emit_gate(strand_name, "gate_disabled", eligible_ready);
                GeneratorPermit::Ordinary
            }
        }
    }

    /// Count the eligible-ready frontier after status, assignee, label, and
    /// exclusion filters. Dependency, retry, and resource eligibility are
    /// applied by the backend's own ready query, so anything returned here has
    /// already survived them.
    async fn count_eligible_ready(
        &self,
        store: &dyn BeadStore,
        exclusions: &HashSet<BeadId>,
    ) -> Result<usize> {
        let filters = Filters {
            assignee: None,
            exclude_labels: self.exclude_labels.clone(),
            exclude_ids: exclusions.clone(),
        };
        let candidates = store.ready(&filters).await?;
        let now = Utc::now();
        Ok(candidates
            .iter()
            .filter(|bead| {
                bead.status == BeadStatus::Open
                    && bead.assignee.is_none()
                    && !exclusions.contains(&bead.id)
                    && !crate::internal::is_internal_artifact(bead)
                    && !self.held_by_label_policy(bead, now)
            })
            .count())
    }

    /// Count open generated proposals still awaiting admission.
    ///
    /// These are exactly the beads [`Self::held_by_label_policy`] keeps out
    /// of the eligible-ready count, so they are read from the full inventory
    /// rather than the ready frontier.
    async fn count_pending_generated_proposals(&self, store: &dyn BeadStore) -> Result<usize> {
        let inventory = store
            .list_all()
            .await
            .context("failed to list the workspace inventory")?;
        Ok(inventory
            .iter()
            .filter(|bead| bead.status == BeadStatus::Open && is_pending_generated_proposal(bead))
            .count())
    }

    fn held_by_label_policy(&self, bead: &crate::types::Bead, now: chrono::DateTime<Utc>) -> bool {
        let expired_marking = crate::bead_store::expired_quarantine_marking(&bead.labels, now);
        if crate::bead_store::active_quarantine_until(bead, now).is_some() {
            return true;
        }

        bead.labels.iter().any(|raw| {
            let label = raw.trim().to_ascii_lowercase();
            let deferred =
                crate::deferral::holds(raw, now) && !(expired_marking && label == "deferred");
            let manual_block = matches!(
                label.as_str(),
                "blocked" | "manual_blocked" | "manual-blocked" | "blocked:manual"
            );
            let human = label == "human"
                || label == "proposal-pending"
                || label.starts_with("human:")
                || label == "human-owned"
                || label == "owner:human";
            let configured = self
                .exclude_labels
                .iter()
                .any(|excluded| excluded.trim().eq_ignore_ascii_case(&label))
                && !(expired_marking && label == "deferred");
            deferred || manual_block || human || configured
        })
    }

    fn emit_gate(&self, strand_name: &str, reason: &str, eligible_ready: usize) {
        let _ = self.telemetry.emit(
            EventKind::GenerationGateEvaluated {
                strand_name: strand_name.to_string(),
                workspace: self.workspace.display().to_string(),
                reason: reason.to_string(),
                eligible_ready,
                low_water_reserve: self.config.low_water_reserve,
            },
            Utc::now(),
        );
    }

    fn lease_paths(&self, strand_name: &str) -> (PathBuf, PathBuf) {
        let mut hasher = Sha256::new();
        hasher.update(self.workspace.display().to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(strand_name.as_bytes());
        let digest = hasher.finalize();
        let key = digest
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let base = self.state_dir.join("generator-leases").join(key);
        (base.with_extension("lock"), base.with_extension("json"))
    }

    /// Acquire the workspace-plus-strand lease under an exclusive flock.
    ///
    /// Returns `Ok(None)` when another worker still holds an unexpired lease.
    /// A won lease is held until its TTL expires — generation is deliberately
    /// not released early, so the TTL also bounds how often a generator that
    /// found no new work can be asked again.
    fn acquire_lease(&self, strand_name: &str) -> Result<Option<String>> {
        let (lock_path, lease_path) = self.lease_paths(strand_name);
        let parent = lock_path
            .parent()
            .context("generator lease lock has no parent directory")?;
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create generator lease dir: {}", parent.display())
        })?;

        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| {
                format!(
                    "failed to open generator lease lock: {}",
                    lock_path.display()
                )
            })?;
        lock_file
            .lock_exclusive()
            .with_context(|| format!("failed to lock generator lease: {}", lock_path.display()))?;

        let now = Utc::now().timestamp();
        let active = read_lease(&lease_path)
            .ok()
            .flatten()
            .is_some_and(|record| record.expires_at_unix > now);
        if active {
            FileExt::unlock(&lock_file).context("failed to unlock generator lease")?;
            return Ok(None);
        }

        let fencing_token = format!(
            "{}-{}-{}",
            self.owner,
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let record = LeaseRecord {
            owner: self.owner.clone(),
            fencing_token: fencing_token.clone(),
            expires_at_unix: now.saturating_add(self.config.lease_ttl_secs as i64),
        };
        let encoded = serde_json::to_vec(&record).context("failed to encode generator lease")?;
        std::fs::write(&lease_path, encoded).with_context(|| {
            format!(
                "failed to persist generator lease: {}",
                lease_path.display()
            )
        })?;
        FileExt::unlock(&lock_file).context("failed to unlock generator lease")?;
        Ok(Some(fencing_token))
    }

    /// Shorten a won lease after the creator itself fails to launch or return.
    ///
    /// Keeping a brief failure lease prevents every exhausted worker from
    /// retrying the same broken provider immediately, while avoiding the full
    /// successful/no-gap cooldown after a failure that created no work. The
    /// fencing token makes a stale worker unable to shorten a newer owner's
    /// lease.
    pub(super) fn mark_creator_failed(&self, strand_name: &str, fencing_token: &str) {
        if let Err(error) = self.shorten_failed_lease(strand_name, fencing_token) {
            tracing::warn!(
                strand = strand_name,
                workspace = %self.workspace.display(),
                error = %error,
                "failed to shorten generation lease after creator failure"
            );
        }
    }

    fn shorten_failed_lease(&self, strand_name: &str, fencing_token: &str) -> Result<()> {
        let (lock_path, lease_path) = self.lease_paths(strand_name);
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| {
                format!(
                    "failed to open generator lease lock: {}",
                    lock_path.display()
                )
            })?;
        lock_file
            .lock_exclusive()
            .with_context(|| format!("failed to lock generator lease: {}", lock_path.display()))?;

        let Some(mut record) = read_lease(&lease_path)? else {
            FileExt::unlock(&lock_file).context("failed to unlock generator lease")?;
            return Ok(());
        };
        if record.fencing_token == fencing_token {
            let failure_retry_secs = self.config.lease_ttl_secs.clamp(1, 30);
            record.expires_at_unix = Utc::now()
                .timestamp()
                .saturating_add(failure_retry_secs as i64);
            let encoded =
                serde_json::to_vec(&record).context("failed to encode generator lease")?;
            std::fs::write(&lease_path, encoded).with_context(|| {
                format!(
                    "failed to persist shortened generator lease: {}",
                    lease_path.display()
                )
            })?;
        }

        FileExt::unlock(&lock_file).context("failed to unlock generator lease")?;
        Ok(())
    }
}

/// Whether a bead is a generator-filed proposal that has not been admitted.
fn is_pending_generated_proposal(bead: &crate::types::Bead) -> bool {
    let has = |wanted: &str| {
        bead.labels
            .iter()
            .any(|label| label.trim().eq_ignore_ascii_case(wanted))
    };
    has(PROPOSAL_PENDING_LABEL) && GENERATED_PROPOSAL_LABELS.iter().any(|label| has(label))
}

fn read_lease(path: &Path) -> Result<Option<LeaseRecord>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("failed to parse generator lease: {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("failed to read generator lease: {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::RepairReport;
    use crate::types::{Bead, ClaimResult};
    use chrono::Utc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Store whose ready query can be made to fail, so a gate evaluation
    /// error can be distinguished from a genuinely empty frontier.
    struct GateStore {
        ready_beads: Vec<Bead>,
        all_beads: Vec<Bead>,
        fail_ready: AtomicBool,
        fail_list: AtomicBool,
    }

    impl GateStore {
        fn healthy(ready_beads: Vec<Bead>) -> Self {
            Self {
                ready_beads,
                all_beads: Vec::new(),
                fail_ready: AtomicBool::new(false),
                fail_list: AtomicBool::new(false),
            }
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for GateStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            if self.fail_list.load(Ordering::SeqCst) {
                anyhow::bail!("inventory query unavailable");
            }
            Ok(self.all_beads.clone())
        }

        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            if self.fail_ready.load(Ordering::SeqCst) {
                anyhow::bail!("ready query unavailable");
            }
            Ok(self.ready_beads.clone())
        }

        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("not implemented")
        }

        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }

        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "empty".to_string(),
            })
        }

        async fn release(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        async fn block(&self, _id: &BeadId) -> Result<()> {
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
            anyhow::bail!("must not create target work")
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

        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }

        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }

        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }

        fn has_valid_store(&self) -> bool {
            true
        }
    }

    fn bead(id: &str) -> Bead {
        Bead {
            id: BeadId::from(id),
            title: id.to_string(),
            body: None,
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: Vec::new(),
            workspace: PathBuf::from("/workspace"),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn gate(state_dir: &Path, ttl: u64, owner: &str) -> GeneratorGate {
        GeneratorGate::new(
            GenerationConfig {
                enabled: true,
                low_water_reserve: 2,
                lease_ttl_secs: ttl,
                proposal_backlog_cap: 10,
            },
            PathBuf::from("/workspace"),
            state_dir.to_path_buf(),
            Vec::new(),
            Telemetry::new(owner.to_string()),
        )
    }

    fn gate_with_reserve(state_dir: &Path, reserve: usize) -> GeneratorGate {
        GeneratorGate::new(
            GenerationConfig {
                enabled: true,
                low_water_reserve: reserve,
                lease_ttl_secs: 300,
                proposal_backlog_cap: 10,
            },
            PathBuf::from("/workspace"),
            state_dir.to_path_buf(),
            Vec::new(),
            Telemetry::new("worker-a".to_string()),
        )
    }

    fn disabled_gate(state_dir: &Path) -> GeneratorGate {
        GeneratorGate::new(
            GenerationConfig {
                enabled: false,
                low_water_reserve: 2,
                lease_ttl_secs: 300,
                proposal_backlog_cap: 10,
            },
            PathBuf::from("/workspace"),
            state_dir.to_path_buf(),
            Vec::new(),
            Telemetry::new("worker-a".to_string()),
        )
    }

    /// A gate with an explicit proposal-backlog cap whose telemetry lands in
    /// `log_dir`, so a test can assert the reason the gate reported.
    fn gate_with_cap(state_dir: &Path, log_dir: &Path, owner: &str, cap: usize) -> GeneratorGate {
        let telemetry = Telemetry::with_log_dir(owner.to_string(), log_dir);
        telemetry.start();
        GeneratorGate::new(
            GenerationConfig {
                enabled: true,
                low_water_reserve: 5,
                lease_ttl_secs: 300,
                proposal_backlog_cap: cap,
            },
            PathBuf::from("/workspace"),
            state_dir.to_path_buf(),
            Vec::new(),
            telemetry,
        )
    }

    /// An unadmitted proposal exactly as the named generator files it.
    fn proposal(id: &str, provenance: &str) -> Bead {
        let mut proposal = bead(id);
        proposal.labels = vec![
            provenance.to_string(),
            "proposal-pending".to_string(),
            "human".to_string(),
        ];
        proposal
    }

    /// Drain the gate's telemetry and return every reported gate reason.
    async fn gate_reasons(gate: &GeneratorGate, log_dir: &Path) -> Vec<String> {
        gate.telemetry.shutdown().await;
        let mut reasons = Vec::new();
        for entry in std::fs::read_dir(log_dir).unwrap().flatten() {
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            for line in std::fs::read_to_string(entry.path()).unwrap().lines() {
                let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if event["event_type"] == "generation.gate_evaluated" {
                    if let Some(reason) = event["data"]["reason"].as_str() {
                        reasons.push(reason.to_string());
                    }
                }
            }
        }
        reasons
    }

    #[tokio::test]
    async fn simultaneous_empty_workers_get_one_generation_lease() {
        let dir = tempfile::tempdir().unwrap();
        let first = gate(dir.path(), 300, "worker-a");
        let second = gate(dir.path(), 300, "worker-b");
        let store = GateStore::healthy(Vec::new());

        let first_permit = first.prepare("weave", &store, &HashSet::new()).await;
        let second_permit = second.prepare("weave", &store, &HashSet::new()).await;

        assert!(matches!(first_permit, GeneratorPermit::LowWater { .. }));
        assert_eq!(second_permit, GeneratorPermit::Contended);
    }

    #[tokio::test]
    async fn expired_generation_lease_can_be_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let first = gate(dir.path(), 300, "worker-a");
        let second = gate(dir.path(), 300, "worker-b");
        let (_, lease_path) = first.lease_paths("weave");
        std::fs::create_dir_all(lease_path.parent().unwrap()).unwrap();
        std::fs::write(
            lease_path,
            serde_json::to_vec(&LeaseRecord {
                owner: "worker-a".to_string(),
                fencing_token: "expired".to_string(),
                expires_at_unix: Utc::now().timestamp() - 1,
            })
            .unwrap(),
        )
        .unwrap();
        let store = GateStore::healthy(Vec::new());

        let permit = second.prepare("weave", &store, &HashSet::new()).await;
        assert!(matches!(permit, GeneratorPermit::LowWater { .. }));
    }

    #[tokio::test]
    async fn creator_failure_shortens_only_the_matching_lease() {
        let dir = tempfile::tempdir().unwrap();
        let gate = gate(dir.path(), 300, "worker-a");
        let store = GateStore::healthy(Vec::new());

        let GeneratorPermit::LowWater { fencing_token } =
            gate.prepare("weave", &store, &HashSet::new()).await
        else {
            panic!("empty frontier should acquire the generation lease");
        };
        let (_, lease_path) = gate.lease_paths("weave");
        let original = read_lease(&lease_path).unwrap().unwrap();

        gate.mark_creator_failed("weave", "stale-token");
        assert_eq!(
            read_lease(&lease_path).unwrap().unwrap().expires_at_unix,
            original.expires_at_unix,
            "a stale creator must not alter the current lease"
        );

        gate.mark_creator_failed("weave", &fencing_token);
        let shortened = read_lease(&lease_path).unwrap().unwrap();
        let remaining = shortened.expires_at_unix - Utc::now().timestamp();
        assert!(
            (1..=30).contains(&remaining),
            "failed creator should retain only the short retry lease; remaining={remaining}"
        );
    }

    #[tokio::test]
    async fn leases_are_scoped_by_workspace_and_strand() {
        let dir = tempfile::tempdir().unwrap();
        let first = gate(dir.path(), 300, "worker-a");
        let mut other_workspace = gate(dir.path(), 300, "worker-b");
        other_workspace.workspace = PathBuf::from("/other-workspace");
        let store = GateStore::healthy(Vec::new());

        assert!(matches!(
            first.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::LowWater { .. }
        ));
        assert!(matches!(
            first.prepare("pulse", &store, &HashSet::new()).await,
            GeneratorPermit::LowWater { .. }
        ));
        assert!(matches!(
            other_workspace
                .prepare("weave", &store, &HashSet::new())
                .await,
            GeneratorPermit::LowWater { .. }
        ));
    }

    #[tokio::test]
    async fn healthy_eligible_backlog_above_reserve_keeps_ordinary_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        let gate = gate(dir.path(), 300, "worker-a");
        let store = GateStore::healthy(vec![bead("one"), bead("two")]);

        assert_eq!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::Ordinary
        );
    }

    #[tokio::test]
    async fn raw_open_beads_do_not_count_when_ready_frontier_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let gate = gate(dir.path(), 300, "worker-a");
        let mut store = GateStore::healthy(Vec::new());
        store.all_beads = vec![bead("dependency-blocked")];

        assert!(matches!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::LowWater { .. }
        ));
    }

    #[tokio::test]
    async fn reserve_of_zero_disables_the_override() {
        let dir = tempfile::tempdir().unwrap();
        let gate = gate_with_reserve(dir.path(), 0);
        let store = GateStore::healthy(Vec::new());

        assert_eq!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::Ordinary
        );
    }

    #[tokio::test]
    async fn disabled_gate_keeps_ordinary_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        let gate = disabled_gate(dir.path());
        let store = GateStore::healthy(Vec::new());

        assert_eq!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::Ordinary
        );
    }

    #[tokio::test]
    async fn gate_evaluation_failure_degrades_to_ordinary_instead_of_blocking_the_generator() {
        let dir = tempfile::tempdir().unwrap();
        let gate = gate(dir.path(), 300, "worker-a");
        let store = GateStore::healthy(Vec::new());
        store.fail_ready.store(true, Ordering::SeqCst);

        assert_eq!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::Ordinary
        );
    }

    #[tokio::test]
    async fn assigned_and_excluded_beads_do_not_count_as_eligible() {
        let dir = tempfile::tempdir().unwrap();
        let gate = GeneratorGate::new(
            GenerationConfig {
                enabled: true,
                low_water_reserve: 2,
                lease_ttl_secs: 300,
                proposal_backlog_cap: 10,
            },
            PathBuf::from("/workspace"),
            dir.path().to_path_buf(),
            vec!["human".to_string()],
            Telemetry::new("worker-a".to_string()),
        );

        let mut assigned = bead("assigned");
        assigned.assignee = Some("someone-else".to_string());
        let mut excluded = bead("excluded");
        excluded.labels = vec!["human".to_string()];

        let mut store = GateStore::healthy(vec![assigned, excluded]);
        store.all_beads = Vec::new();

        assert!(matches!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::LowWater { .. }
        ));
    }

    #[tokio::test]
    async fn default_hard_labels_do_not_make_an_exhausted_frontier_look_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let gate = gate(dir.path(), 300, "worker-a");
        let mut deferred = bead("deferred");
        deferred.labels = vec!["deferred".to_string(), "failure-count:1".to_string()];
        let mut human = bead("human");
        human.labels = vec!["human:review".to_string()];
        let store = GateStore::healthy(vec![deferred, human]);

        assert!(matches!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::LowWater { .. }
        ));
    }

    #[tokio::test]
    async fn expired_automatic_quarantine_counts_toward_the_frontier() {
        let dir = tempfile::tempdir().unwrap();
        let gate = gate_with_reserve(dir.path(), 1);
        let mut recovered = bead("recovered");
        recovered.labels = vec![
            "deferred".to_string(),
            "failure-count:1".to_string(),
            format!(
                "quarantine-until:{}",
                (Utc::now() - chrono::Duration::minutes(5)).to_rfc3339()
            ),
        ];
        let store = GateStore::healthy(vec![recovered]);

        assert_eq!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::Ordinary
        );
    }

    #[tokio::test]
    async fn below_reserve_with_a_full_proposal_backlog_withholds_the_override() {
        // needle-b117f398: the frontier is below the reserve only because the
        // generator's own proposals are held for admission. Generating again
        // cannot raise it, so the override must not fire.
        let dir = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        let gate = gate_with_cap(dir.path(), logs.path(), "worker-a", 3);
        let proposals = vec![
            proposal("weave-one", "weave-generated"),
            proposal("weave-two", "weave-generated"),
            proposal("pulse-one", "pulse-finding"),
        ];
        // The held proposals are on the ready frontier too, which proves they
        // are excluded from the eligible count rather than absent from it.
        let mut store = GateStore::healthy(proposals.clone());
        store.all_beads = proposals;

        assert_eq!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::Ordinary,
            "a backlog at the cap must fall back to the ordinary cooldown"
        );

        // The won lease is kept, so other workers do not re-run the inventory
        // query for this workspace and strand until it expires.
        let other_logs = tempfile::tempdir().unwrap();
        let other = gate_with_cap(dir.path(), other_logs.path(), "worker-b", 3);
        assert_eq!(
            other.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::Contended
        );

        assert_eq!(
            gate_reasons(&other, other_logs.path()).await,
            vec!["contended".to_string()]
        );

        let reasons = gate_reasons(&gate, logs.path()).await;
        assert!(
            reasons.iter().any(|reason| reason == "proposal_backlog"),
            "the suppression must be reported as proposal_backlog: {reasons:?}"
        );
        assert!(
            !reasons.iter().any(|reason| reason == "low_water"),
            "no low-water override may be reported: {reasons:?}"
        );
    }

    #[tokio::test]
    async fn genuinely_empty_workspace_still_gets_the_low_water_override() {
        let dir = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        let gate = gate_with_cap(dir.path(), logs.path(), "worker-a", 3);
        let store = GateStore::healthy(Vec::new());

        assert!(matches!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::LowWater { .. }
        ));
        let reasons = gate_reasons(&gate, logs.path()).await;
        assert_eq!(reasons, vec!["low_water".to_string()]);
    }

    #[tokio::test]
    async fn proposal_backlog_below_the_cap_still_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        let gate = gate_with_cap(dir.path(), logs.path(), "worker-a", 3);
        let mut store = GateStore::healthy(Vec::new());
        store.all_beads = vec![
            proposal("weave-one", "weave-generated"),
            proposal("pulse-one", "pulse-finding"),
        ];

        assert!(matches!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::LowWater { .. }
        ));
    }

    #[tokio::test]
    async fn only_open_unadmitted_generated_proposals_count_toward_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        let gate = gate_with_cap(dir.path(), logs.path(), "worker-a", 1);

        let mut admitted = bead("admitted");
        admitted.labels = vec!["weave-generated".to_string()];
        let mut closed = proposal("closed", "weave-generated");
        closed.status = BeadStatus::Closed;
        let mut deferred = proposal("deferred", "pulse-finding");
        deferred.status = BeadStatus::Deferred;
        let unravel = proposal("unravel", "unravel-proposal");
        let mut unattributed = bead("unattributed");
        unattributed.labels = vec!["proposal-pending".to_string(), "human".to_string()];

        let mut store = GateStore::healthy(Vec::new());
        store.all_beads = vec![admitted, closed, deferred, unravel, unattributed];

        assert!(matches!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::LowWater { .. }
        ));
    }

    #[tokio::test]
    async fn zero_proposal_backlog_cap_disables_the_check() {
        // A cap of zero restores the pre-cap behaviour: the backlog is never
        // counted, so even an inventory that cannot be read does not matter.
        let dir = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        let gate = gate_with_cap(dir.path(), logs.path(), "worker-a", 0);
        let mut store = GateStore::healthy(Vec::new());
        store.all_beads = (0..50)
            .map(|index| proposal(&format!("weave-{index}"), "weave-generated"))
            .collect();
        store.fail_list.store(true, Ordering::SeqCst);

        assert!(matches!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::LowWater { .. }
        ));
    }

    #[tokio::test]
    async fn proposal_backlog_count_failure_degrades_to_ordinary() {
        let dir = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        let gate = gate_with_cap(dir.path(), logs.path(), "worker-a", 3);
        let store = GateStore::healthy(Vec::new());
        store.fail_list.store(true, Ordering::SeqCst);

        assert_eq!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::Ordinary
        );
        let reasons = gate_reasons(&gate, logs.path()).await;
        assert_eq!(reasons, vec!["gate_disabled".to_string()]);
    }

    #[tokio::test]
    async fn healthy_reserve_never_queries_the_proposal_backlog() {
        // The inventory query is only paid for on the low-water path.
        let dir = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        let gate = gate_with_cap(dir.path(), logs.path(), "worker-a", 3);
        let store = GateStore::healthy(
            (0..5)
                .map(|index| bead(&format!("ready-{index}")))
                .collect(),
        );
        store.fail_list.store(true, Ordering::SeqCst);

        assert_eq!(
            gate.prepare("weave", &store, &HashSet::new()).await,
            GeneratorPermit::Ordinary
        );
        let reasons = gate_reasons(&gate, logs.path()).await;
        assert_eq!(reasons, vec!["backlog_healthy".to_string()]);
    }

    #[test]
    fn only_the_low_water_permit_bypasses_a_cooldown() {
        assert!(GeneratorPermit::LowWater {
            fencing_token: "t".to_string()
        }
        .bypasses_cooldown());
        assert!(!GeneratorPermit::Ordinary.bypasses_cooldown());
        assert!(!GeneratorPermit::Contended.bypasses_cooldown());
    }
}
