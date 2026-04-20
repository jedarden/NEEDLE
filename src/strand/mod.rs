//! Strand waterfall: ordered sequence of selection strategies.
//!
//! The StrandRunner evaluates strands in priority order. The first strand
//! that yields a candidate wins. Strands are stateless — they receive queue
//! state and return a candidate or nothing.
//!
//! Depends on: `types`, `config`, `bead_store`.

mod explore;
mod knot;
mod mend;
mod pluck;
pub mod pulse;
pub mod reflect;
pub mod unravel;
pub mod weave;

use std::time::Instant;

use anyhow::Result;

use crate::bead_store::BeadStore;
use crate::config::Config;
use crate::types::{Bead, StrandResult};

pub use explore::ExploreStrand;
pub use knot::KnotStrand;
pub use mend::MendStrand;
pub use pluck::PluckStrand;
pub use pulse::PulseStrand;
pub use reflect::ReflectStrand;
pub use unravel::{UnravelAgent, UnravelStrand};
pub use weave::{CliWeaveAgent, WeaveAgent, WeaveStrand};

/// A single selection strategy in the waterfall.
#[async_trait::async_trait]
pub trait Strand: Send + Sync {
    /// Human-readable name for telemetry.
    fn name(&self) -> &str;

    /// Evaluate this strand against the current queue state.
    async fn evaluate(&self, store: &dyn BeadStore) -> StrandResult;
}

/// Runs strands in order, returning the first candidate found.
pub struct StrandRunner {
    strands: Vec<Box<dyn Strand>>,
}

impl StrandRunner {
    pub fn new(strands: Vec<Box<dyn Strand>>) -> Self {
        StrandRunner { strands }
    }

    /// Build the default strand waterfall from config.
    ///
    /// The waterfall order is:
    /// Pluck → Mend → Explore → Weave → Unravel → Pulse → Reflect → Knot.
    pub fn from_config(
        config: &Config,
        worker_id: &str,
        registry: crate::registry::Registry,
        telemetry: crate::telemetry::Telemetry,
    ) -> Self {
        let pluck = PluckStrand::new(config.strands.pluck.exclude_labels.clone());

        let heartbeat_dir = config.workspace.home.join("state").join("heartbeats");
        let heartbeat_ttl = std::time::Duration::from_secs(config.health.heartbeat_ttl_secs);
        let lock_dir = std::path::PathBuf::from("/tmp");
        let log_dir = config
            .telemetry
            .file_sink
            .log_dir
            .clone()
            .unwrap_or_else(|| config.workspace.home.join("logs"));
        let retention_days = config.telemetry.file_sink.retention_days;
        let traces_dir = config.workspace.default.join(".beads").join("traces");
        let trace_retention_failed_days = config.strands.learning.trace_retention_failed_days;
        let trace_retention_success_days = config.strands.learning.trace_retention_success_days;
        let mend = MendStrand::new(
            config.strands.mend.clone(),
            heartbeat_dir,
            heartbeat_ttl,
            lock_dir,
            worker_id.to_string(),
            registry,
            telemetry.clone(),
            log_dir,
            retention_days,
            traces_dir,
            trace_retention_failed_days,
            trace_retention_success_days,
            config.workspace.default.clone(),
            config.strands.learning.max_learnings,
        );

        let explore = ExploreStrand::new(
            config.strands.explore.clone(),
            config.workspace.default.clone(),
        );

        let state_base = config.workspace.home.join("state");

        let weave = WeaveStrand::new(
            config.strands.weave.clone(),
            config.workspace.default.clone(),
            state_base.join("weave"),
            Box::new(CliWeaveAgent::new(config.agent.default.clone())),
        );

        let unravel = UnravelStrand::new(
            config.strands.unravel.clone(),
            config.workspace.default.clone(),
            state_base.join("unravel"),
            Box::new(unravel::CliUnravelAgent::new(config.agent.default.clone())),
            telemetry.clone(),
        );

        let pulse = PulseStrand::new(
            config.strands.pulse.clone(),
            config.workspace.default.clone(),
            state_base.join("pulse"),
            telemetry.clone(),
        );

        // Create the extraction agent if configured.
        let reflect_agent = config
            .strands
            .reflect
            .extraction_agent
            .as_ref()
            .map(|agent_cmd| {
                Box::new(reflect::CliReflectAgent::new(
                    agent_cmd.clone(),
                    config.strands.reflect.extraction_prompt_template.clone(),
                )) as Box<dyn reflect::ReflectAgent>
            });

        let reflect = ReflectStrand::new(
            config.strands.reflect.clone(),
            config.workspace.default.clone(),
            state_base.join("reflect"),
            telemetry,
            reflect_agent,
        );

        let knot = KnotStrand::new(config.strands.knot.clone());
        StrandRunner {
            strands: vec![
                Box::new(pluck),
                Box::new(mend),
                Box::new(explore),
                Box::new(weave),
                Box::new(unravel),
                Box::new(pulse),
                Box::new(reflect),
                Box::new(knot),
            ],
        }
    }

    /// Run the waterfall, returning the first candidate bead or None.
    ///
    /// Returns the full `Bead` (including its workspace path) so the caller
    /// can create the correct bead store for remote beads found by Explore.
    ///
    /// When a strand returns `WorkCreated`, the waterfall restarts from Pluck.
    /// A restart cap prevents infinite loops (e.g. a strand that always creates
    /// work without producing a claimable bead).
    pub async fn select(&self, store: &dyn BeadStore) -> Result<Option<Bead>> {
        const MAX_RESTARTS: u32 = 3;
        let mut restarts = 0u32;

        'waterfall: loop {
            for strand in &self.strands {
                let start = Instant::now();
                let result = strand.evaluate(store).await;
                let elapsed_ms = start.elapsed().as_millis() as u64;

                match result {
                    StrandResult::BeadFound(beads) => {
                        tracing::info!(
                            strand = strand.name(),
                            candidates = beads.len(),
                            elapsed_ms,
                            "strand found candidates"
                        );
                        if let Some(bead) = beads.into_iter().next() {
                            return Ok(Some(bead));
                        }
                        continue;
                    }
                    StrandResult::WorkCreated => {
                        restarts += 1;
                        tracing::info!(
                            strand = strand.name(),
                            elapsed_ms,
                            restart = restarts,
                            "strand created new work, restarting waterfall"
                        );
                        if restarts > MAX_RESTARTS {
                            tracing::warn!(
                                max_restarts = MAX_RESTARTS,
                                "waterfall restart cap reached, treating as exhausted"
                            );
                            return Ok(None);
                        }
                        continue 'waterfall;
                    }
                    StrandResult::NoWork => {
                        tracing::debug!(
                            strand = strand.name(),
                            elapsed_ms,
                            "strand returned no work"
                        );
                        continue;
                    }
                    StrandResult::Error(e) => {
                        tracing::warn!(
                            strand = strand.name(),
                            error = %e,
                            elapsed_ms,
                            "strand error, continuing to next strand"
                        );
                        continue;
                    }
                }
            }
            // All strands evaluated without finding work or triggering a restart.
            return Ok(None);
        }
    }

    /// Return the names of all configured strands (for telemetry/debugging).
    pub fn strand_names(&self) -> Vec<&str> {
        self.strands.iter().map(|s| s.name()).collect()
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Bead, BeadId};

    /// A stub strand that always returns the given result.
    struct StubStrand {
        name: &'static str,
        result: std::sync::Mutex<Option<StrandResult>>,
    }

    impl StubStrand {
        fn no_work(name: &'static str) -> Self {
            StubStrand {
                name,
                result: std::sync::Mutex::new(Some(StrandResult::NoWork)),
            }
        }

        fn beads(name: &'static str, beads: Vec<Bead>) -> Self {
            StubStrand {
                name,
                result: std::sync::Mutex::new(Some(StrandResult::BeadFound(beads))),
            }
        }

        fn work_created(name: &'static str) -> Self {
            StubStrand {
                name,
                result: std::sync::Mutex::new(Some(StrandResult::WorkCreated)),
            }
        }

        fn error(name: &'static str, msg: &str) -> Self {
            StubStrand {
                name,
                result: std::sync::Mutex::new(Some(StrandResult::Error(
                    crate::types::StrandError::ConfigError(msg.to_string()),
                ))),
            }
        }
    }

    #[async_trait::async_trait]
    impl Strand for StubStrand {
        fn name(&self) -> &str {
            self.name
        }

        async fn evaluate(&self, _store: &dyn BeadStore) -> StrandResult {
            self.result
                .lock()
                .unwrap()
                .take()
                .unwrap_or(StrandResult::NoWork)
        }
    }

    fn make_test_bead(id: &str) -> Bead {
        use chrono::Utc;
        Bead {
            id: BeadId::from(id.to_string()),
            title: format!("Test bead {id}"),
            body: None,
            priority: 1,
            status: crate::types::BeadStatus::Open,
            assignee: None,
            labels: vec![],
            workspace: std::path::PathBuf::from("/tmp/test"),
            dependencies: vec![],
            dependents: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// Stub BeadStore for tests — always returns empty.
    struct EmptyStore;

    #[async_trait::async_trait]
    impl BeadStore for EmptyStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn ready(&self, _filters: &crate::bead_store::Filters) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("not found")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<crate::types::ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn reopen(&self, _id: &BeadId) -> Result<()> {
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
            Ok(BeadId::from("new-bead".to_string()))
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
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn empty_waterfall_returns_none() {
        let runner = StrandRunner::new(vec![]);
        let store = EmptyStore;
        let result = runner.select(&store).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn first_strand_with_beads_wins() {
        let bead = make_test_bead("test-001");
        let runner = StrandRunner::new(vec![
            Box::new(StubStrand::no_work("empty")),
            Box::new(StubStrand::beads("finder", vec![bead])),
        ]);
        let store = EmptyStore;
        let result = runner.select(&store).await.unwrap();
        assert_eq!(
            result.map(|b| b.id),
            Some(BeadId::from("test-001".to_string()))
        );
    }

    #[tokio::test]
    async fn work_created_restarts_waterfall() {
        let runner = StrandRunner::new(vec![
            Box::new(StubStrand::work_created("creator")),
            Box::new(StubStrand::beads(
                "finder",
                vec![make_test_bead("test-002")],
            )),
        ]);
        let store = EmptyStore;
        // WorkCreated restarts the waterfall. On the second pass, "creator"
        // returns NoWork (stub consumed) and "finder" yields the bead.
        let result = runner.select(&store).await.unwrap();
        assert_eq!(
            result.map(|b| b.id),
            Some(BeadId::from("test-002".to_string()))
        );
    }

    #[tokio::test]
    async fn all_no_work_returns_none() {
        let runner = StrandRunner::new(vec![
            Box::new(StubStrand::no_work("s1")),
            Box::new(StubStrand::no_work("s2")),
            Box::new(StubStrand::no_work("s3")),
        ]);
        let store = EmptyStore;
        let result = runner.select(&store).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn strand_names_returns_all_names() {
        let runner = StrandRunner::new(vec![
            Box::new(StubStrand::no_work("alpha")),
            Box::new(StubStrand::no_work("beta")),
        ]);
        assert_eq!(runner.strand_names(), vec!["alpha", "beta"]);
    }

    /// A strand that always returns WorkCreated (never consumed).
    struct AlwaysWorkCreated;

    #[async_trait::async_trait]
    impl Strand for AlwaysWorkCreated {
        fn name(&self) -> &str {
            "always-creates"
        }
        async fn evaluate(&self, _store: &dyn BeadStore) -> StrandResult {
            StrandResult::WorkCreated
        }
    }

    #[tokio::test]
    async fn error_strand_continues_to_next() {
        let bead = make_test_bead("after-error");
        let runner = StrandRunner::new(vec![
            Box::new(StubStrand::error("broken", "something went wrong")),
            Box::new(StubStrand::beads("finder", vec![bead])),
        ]);
        let store = EmptyStore;
        let result = runner.select(&store).await.unwrap();
        assert_eq!(
            result.map(|b| b.id),
            Some(BeadId::from("after-error".to_string()))
        );
    }

    #[tokio::test]
    async fn restart_cap_prevents_infinite_loop() {
        // AlwaysWorkCreated triggers restarts every pass.
        // After MAX_RESTARTS (3), the waterfall should return None.
        let runner = StrandRunner::new(vec![Box::new(AlwaysWorkCreated)]);
        let store = EmptyStore;
        let result = runner.select(&store).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn empty_bead_found_continues_to_next() {
        let bead = make_test_bead("real-bead");
        let runner = StrandRunner::new(vec![
            Box::new(StubStrand::beads("empty-finder", vec![])),
            Box::new(StubStrand::beads("real-finder", vec![bead])),
        ]);
        let store = EmptyStore;
        let result = runner.select(&store).await.unwrap();
        assert_eq!(
            result.map(|b| b.id),
            Some(BeadId::from("real-bead".to_string()))
        );
    }

    #[tokio::test]
    async fn multiple_beads_returns_first() {
        let bead1 = make_test_bead("first");
        let bead2 = make_test_bead("second");
        let runner = StrandRunner::new(vec![Box::new(StubStrand::beads(
            "multi",
            vec![bead1, bead2],
        ))]);
        let store = EmptyStore;
        let result = runner.select(&store).await.unwrap();
        assert_eq!(
            result.map(|b| b.id),
            Some(BeadId::from("first".to_string()))
        );
    }

    #[tokio::test]
    async fn from_config_includes_full_waterfall() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let registry = crate::registry::Registry::new(dir.path());
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let runner = StrandRunner::from_config(&config, "test-worker", registry, telemetry);
        assert_eq!(
            runner.strand_names(),
            vec!["pluck", "mend", "explore", "weave", "unravel", "pulse", "reflect", "knot"]
        );
    }
}
