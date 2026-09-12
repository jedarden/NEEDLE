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

/// What a generator may do in the current waterfall pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GeneratorPermit {
    /// Apply the generator's ordinary cooldown policy. Returned when the gate
    /// is disabled, the reserve is zero, the eligible backlog already meets
    /// the reserve, or the gate could not evaluate the backlog at all.
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
                    && !self.held_by_label_policy(bead, now)
            })
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
    }

    impl GateStore {
        fn healthy(ready_beads: Vec<Bead>) -> Self {
            Self {
                ready_beads,
                all_beads: Vec::new(),
                fail_ready: AtomicBool::new(false),
            }
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for GateStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
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
            },
            PathBuf::from("/workspace"),
            state_dir.to_path_buf(),
            Vec::new(),
            Telemetry::new("worker-a".to_string()),
        )
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
