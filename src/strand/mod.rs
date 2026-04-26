//! Strand waterfall: ordered sequence of selection strategies.
//!
//! The StrandRunner evaluates strands in priority order. The first strand
//! that yields a candidate wins. Strands are stateless — they receive queue
//! state and return a candidate or nothing.
//!
//! Depends on: `types`, `config`, `bead_store`.

mod explore;
mod knot;
pub mod mend;
mod pluck;
pub mod pulse;
pub mod reflect;
pub mod splice;
pub mod unravel;
pub mod weave;

use std::collections::HashSet;
use std::time::Instant;

use anyhow::Result;

use crate::bead_store::BeadStore;
use crate::config::Config;
use crate::span::{attrs, strand_results};
use crate::types::{Bead, BeadId, StrandResult};

/// A single strand evaluation result.
#[derive(Debug, Clone)]
pub struct StrandEvaluation {
    pub strand_name: String,
    pub result: String,
    pub duration_ms: u64,
}

/// Result of a single `StrandRunner::select()` call.
///
/// Carries both the winning candidate (if any) and diagnostic statistics
/// about restarts that occurred during the waterfall — used to populate
/// `worker.exhausted` telemetry with a per-iteration breakdown.
#[derive(Debug, Default)]
pub struct SelectOutcome {
    /// The candidate bead and the strand that found it, or `None` if
    /// all strands returned `NoWork`.
    pub bead: Option<(Bead, String)>,
    /// How many times the waterfall restarted from Pluck (cap = `MAX_RESTARTS`).
    pub waterfall_restarts: u32,
    /// Names of strands that returned `WorkCreated` and triggered a restart.
    /// Duplicate entries are preserved (one per restart event).
    pub restart_triggers: Vec<String>,
    /// All strand evaluations in order, across all waterfall passes.
    /// Each entry is (strand_name, result, duration_ms).
    pub strand_evaluations: Vec<StrandEvaluation>,
    /// The strand span guard for the strand that found the bead.
    /// This keeps the strand span active through the bead lifecycle.
    /// Dropped when the bead lifecycle ends (in do_log).
    #[allow(dead_code)]
    pub strand_span_guard: Option<tracing::span::EnteredSpan>,
}

pub use explore::ExploreStrand;
pub use knot::KnotStrand;
pub use mend::{cleanup_orphaned_in_progress, MendStrand};
pub use pluck::PluckStrand;
pub use pulse::PulseStrand;
pub use reflect::{CliReflectAgent, ReflectAgent, ReflectStrand};
pub use splice::SpliceStrand;
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
    telemetry: crate::telemetry::Telemetry,
}

impl StrandRunner {
    pub fn new(strands: Vec<Box<dyn Strand>>) -> Self {
        StrandRunner {
            strands,
            telemetry: crate::telemetry::Telemetry::new("strand-runner".to_string()),
        }
    }

    /// Build the default strand waterfall from config.
    ///
    /// The waterfall order is:
    /// Pluck → Mend → Explore → Weave → Unravel → Pulse → Reflect → Splice → Knot.
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

        // Create a new Registry instance pointing to the same path for ExploreStrand.
        // We need to get the state_dir_for_explore before moving registry to MendStrand.
        let state_fallback = config.workspace.home.join("state");
        let state_dir_for_explore = registry.path().parent().unwrap_or(&state_fallback);
        let explore_registry = crate::registry::Registry::new(state_dir_for_explore);

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
            config.workspace.home.join("state"),
            config.limits.clone(),
        );

        let state_base = config.workspace.home.join("state");

        let explore = ExploreStrand::new(
            config.strands.explore.clone(),
            config.workspace.default.clone(),
            explore_registry,
            telemetry.clone(),
            worker_id.to_string(),
        );

        let weave = WeaveStrand::new(
            config.strands.weave.clone(),
            config.workspace.default.clone(),
            state_base.join("weave"),
            Box::new(CliWeaveAgent::new(config.agent.default.clone())),
            telemetry.clone(),
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
            telemetry.clone(),
            reflect_agent,
        );

        // Reconstruct heartbeat_dir for Splice (same path used by Mend).
        let splice_heartbeat_dir = config.workspace.home.join("state").join("heartbeats");
        let runner_telemetry = telemetry.clone();
        let splice = SpliceStrand::new(
            config.strands.splice.clone(),
            splice_heartbeat_dir,
            state_base.join("splice"),
            telemetry,
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
                Box::new(splice),
                Box::new(knot),
            ],
            telemetry: runner_telemetry,
        }
    }

    /// Run the waterfall, returning a `SelectOutcome` that carries the winning
    /// candidate (if any) plus restart diagnostics.
    ///
    /// Returns the full `Bead` (including its workspace path) so the caller
    /// can create the correct bead store for remote beads found by Explore.
    /// The accompanying `String` is the name of the strand that produced the
    /// candidate.
    ///
    /// When a strand returns `WorkCreated`, the waterfall restarts from Pluck.
    /// A restart cap prevents infinite loops (e.g. a strand that always creates
    /// work without producing a claimable bead).
    pub async fn select(
        &self,
        store: &dyn BeadStore,
        exclusions: &HashSet<BeadId>,
    ) -> Result<SelectOutcome> {
        const MAX_RESTARTS: u32 = 3;
        let mut restarts = 0u32;
        let mut restart_triggers: Vec<String> = Vec::new();
        let mut strand_evaluations: Vec<StrandEvaluation> = Vec::new();

        'waterfall: loop {
            for strand in &self.strands {
                let strand_name = strand.name().to_string();
                let strand_span = tracing::info_span!(
                    "strand.{}",
                    strand_name,
                    needle.strand.name = %strand_name,
                );
                let strand_enter = strand_span.entered();

                let start = Instant::now();
                let result = strand.evaluate(store).await;
                let elapsed_ms = start.elapsed().as_millis() as u64;

                // Record strand evaluation result as span attribute.
                let (result_str, should_record) = match &result {
                    StrandResult::BeadFound(beads) => {
                        let count = beads.len();
                        (
                            format!("{}({})", strand_results::BEAD_FOUND, count),
                            count > 0,
                        )
                    }
                    StrandResult::WorkCreated => (strand_results::WORK_CREATED.to_string(), true),
                    StrandResult::NoWork => (strand_results::NO_WORK.to_string(), true),
                    StrandResult::Error(_) => (strand_results::ERROR.to_string(), true),
                };
                tracing::Span::current().record(attrs::NEEDLE_STRAND_RESULT, &result_str);
                tracing::Span::current().record(attrs::NEEDLE_STRAND_DURATION_MS, elapsed_ms);

                // Set strand span status: Error for strand errors, Ok for all other results
                if matches!(result, StrandResult::Error(_)) {
                    tracing::Span::current().record("otel.status_code", 2u64);
                    tracing::Span::current().record("otel.status_description", &result_str);
                }

                // Only record evaluations that produced meaningful results.
                // Skip recording empty BeadFound results since they don't
                // represent actual strand activity.
                if should_record {
                    strand_evaluations.push(StrandEvaluation {
                        strand_name: strand_name.clone(),
                        result: result_str.clone(),
                        duration_ms: elapsed_ms,
                    });
                }

                match result {
                    StrandResult::BeadFound(beads) => {
                        // Filter out beads that are in the exclusion set (e.g.
                        // recently race-lost).  This prevents the waterfall from
                        // immediately re-selecting a bead that just lost a claim
                        // race to another worker.
                        let original_count = beads.len();
                        let filtered: Vec<Bead> = beads
                            .into_iter()
                            .filter(|b| !exclusions.contains(&b.id))
                            .collect();
                        let excluded_count = original_count.saturating_sub(filtered.len());

                        // Record queue depth for the Pluck strand (after filtering).
                        // This samples the current queue depth for the needle.queue.depth
                        // observable gauge, which is measured at strand evaluation.
                        // We report depth per priority level to enable filtered views.
                        if strand_name == "pluck" {
                            use std::collections::HashMap;
                            let mut depths: HashMap<u8, u64> = HashMap::new();
                            for bead in &filtered {
                                *depths.entry(bead.priority).or_insert(0) += 1;
                            }
                            self.telemetry.record_queue_depth(depths);
                        }

                        if let Err(e) =
                            self.telemetry
                                .emit(crate::telemetry::EventKind::StrandEvaluated {
                                    strand_name: strand_name.clone(),
                                    result: "bead_found".to_string(),
                                    duration_ms: elapsed_ms,
                                })
                        {
                            tracing::warn!(
                                strand = %strand_name,
                                error = %e,
                                "failed to emit strand evaluated telemetry"
                            );
                        }
                        tracing::info!(
                            strand = %strand_name,
                            candidates = filtered.len(),
                            excluded = excluded_count,
                            elapsed_ms,
                            "strand found candidates"
                        );
                        if let Some(bead) = filtered.into_iter().next() {
                            // Keep the strand span active through the bead lifecycle.
                            // The span guard will be dropped when the bead lifecycle ends.
                            return Ok(SelectOutcome {
                                bead: Some((bead, strand_name.clone())),
                                waterfall_restarts: restarts,
                                restart_triggers,
                                strand_evaluations,
                                strand_span_guard: Some(strand_enter),
                            });
                        }
                        continue;
                    }
                    StrandResult::WorkCreated => {
                        if let Err(e) =
                            self.telemetry
                                .emit(crate::telemetry::EventKind::StrandEvaluated {
                                    strand_name: strand_name.clone(),
                                    result: "work_created".to_string(),
                                    duration_ms: elapsed_ms,
                                })
                        {
                            tracing::warn!(
                                strand = %strand_name,
                                error = %e,
                                "failed to emit strand evaluated telemetry"
                            );
                        }
                        restarts += 1;
                        restart_triggers.push(strand_name.clone());
                        if restarts > MAX_RESTARTS {
                            tracing::warn!(
                                strand = %strand_name,
                                max_restarts = MAX_RESTARTS,
                                "waterfall restart cap reached, continuing to evaluate remaining strands"
                            );
                            // Do NOT return None — continue evaluating remaining strands
                            // so every strand emits telemetry and the operator can see
                            // why the worker is idle.
                            continue;
                        }
                        tracing::info!(
                            strand = %strand_name,
                            elapsed_ms,
                            restart = restarts,
                            "strand created new work, restarting waterfall"
                        );
                        continue 'waterfall;
                    }
                    StrandResult::NoWork => {
                        if let Err(e) =
                            self.telemetry
                                .emit(crate::telemetry::EventKind::StrandEvaluated {
                                    strand_name: strand_name.clone(),
                                    result: "no_work".to_string(),
                                    duration_ms: elapsed_ms,
                                })
                        {
                            tracing::warn!(
                                strand = %strand_name,
                                error = %e,
                                "failed to emit strand evaluated telemetry"
                            );
                        }
                        tracing::info!(
                            strand = %strand_name,
                            elapsed_ms,
                            "strand returned no work"
                        );
                        continue;
                    }
                    StrandResult::Error(e) => {
                        if let Err(te) =
                            self.telemetry
                                .emit(crate::telemetry::EventKind::StrandEvaluated {
                                    strand_name: strand_name.clone(),
                                    result: "error".to_string(),
                                    duration_ms: elapsed_ms,
                                })
                        {
                            tracing::warn!(
                                strand = %strand_name,
                                error = %te,
                                "failed to emit strand evaluated telemetry"
                            );
                        }
                        tracing::warn!(
                            strand = %strand_name,
                            error = %e,
                            elapsed_ms,
                            "strand error, continuing to next strand"
                        );
                        continue;
                    }
                }
            }
            // All strands evaluated without finding work or triggering a restart.
            return Ok(SelectOutcome {
                bead: None,
                waterfall_restarts: restarts,
                restart_triggers,
                strand_evaluations,
                strand_span_guard: None,
            });
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
        async fn flush(&self) -> Result<()> {
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
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn empty_waterfall_returns_none() {
        let runner = StrandRunner::new(vec![]);
        let store = EmptyStore;
        let outcome = runner.select(&store, &HashSet::new()).await.unwrap();
        assert!(outcome.bead.is_none());
        assert_eq!(outcome.waterfall_restarts, 0);
    }

    #[tokio::test]
    async fn first_strand_with_beads_wins() {
        let bead = make_test_bead("test-001");
        let runner = StrandRunner::new(vec![
            Box::new(StubStrand::no_work("empty")),
            Box::new(StubStrand::beads("finder", vec![bead])),
        ]);
        let store = EmptyStore;
        let outcome = runner.select(&store, &HashSet::new()).await.unwrap();
        let (bead, strand_name) = outcome.bead.unwrap();
        assert_eq!(bead.id, BeadId::from("test-001".to_string()));
        assert_eq!(strand_name, "finder");
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
        let outcome = runner.select(&store, &HashSet::new()).await.unwrap();
        assert_eq!(
            outcome.bead.map(|(b, _)| b.id),
            Some(BeadId::from("test-002".to_string()))
        );
        assert_eq!(outcome.waterfall_restarts, 1);
        assert_eq!(outcome.restart_triggers, vec!["creator"]);
    }

    #[tokio::test]
    async fn all_no_work_returns_none() {
        let runner = StrandRunner::new(vec![
            Box::new(StubStrand::no_work("s1")),
            Box::new(StubStrand::no_work("s2")),
            Box::new(StubStrand::no_work("s3")),
        ]);
        let store = EmptyStore;
        let outcome = runner.select(&store, &HashSet::new()).await.unwrap();
        assert!(outcome.bead.is_none());
        assert_eq!(outcome.waterfall_restarts, 0);
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
        let outcome = runner.select(&store, &HashSet::new()).await.unwrap();
        assert_eq!(
            outcome.bead.map(|(b, _)| b.id),
            Some(BeadId::from("after-error".to_string()))
        );
    }

    #[tokio::test]
    async fn restart_cap_prevents_infinite_loop() {
        // AlwaysWorkCreated triggers restarts every pass.
        // After MAX_RESTARTS (3), the waterfall should return None.
        let runner = StrandRunner::new(vec![Box::new(AlwaysWorkCreated)]);
        let store = EmptyStore;
        let exclusions = HashSet::new();
        let outcome = runner.select(&store, &exclusions).await.unwrap();
        assert!(outcome.bead.is_none());
        assert_eq!(outcome.waterfall_restarts, 4); // 3 restarts + 1 cap-exceeded
        assert_eq!(
            outcome.restart_triggers,
            vec![
                "always-creates",
                "always-creates",
                "always-creates",
                "always-creates"
            ]
        );
    }

    /// Strand that increments an external counter each time it is evaluated.
    struct CountingStrand {
        name: &'static str,
        count: std::sync::Arc<std::sync::atomic::AtomicU32>,
        returns_work_created: bool,
    }

    impl CountingStrand {
        fn no_work(
            name: &'static str,
            count: std::sync::Arc<std::sync::atomic::AtomicU32>,
        ) -> Self {
            CountingStrand {
                name,
                count,
                returns_work_created: false,
            }
        }

        fn work_created(
            name: &'static str,
            count: std::sync::Arc<std::sync::atomic::AtomicU32>,
        ) -> Self {
            CountingStrand {
                name,
                count,
                returns_work_created: true,
            }
        }
    }

    #[async_trait::async_trait]
    impl Strand for CountingStrand {
        fn name(&self) -> &str {
            self.name
        }

        async fn evaluate(&self, _store: &dyn BeadStore) -> StrandResult {
            self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.returns_work_created {
                StrandResult::WorkCreated
            } else {
                StrandResult::NoWork
            }
        }
    }

    #[tokio::test]
    async fn restart_cap_still_evaluates_remaining_strands() {
        // When a strand repeatedly returns WorkCreated, the waterfall restarts.
        // After MAX_RESTARTS, the remaining strands should still be evaluated
        // so that operators see telemetry for every strand in the cycle.
        let creator_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let observer_a_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let observer_b_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));

        let runner = StrandRunner::new(vec![
            Box::new(CountingStrand::work_created(
                "creator",
                creator_count.clone(),
            )),
            Box::new(CountingStrand::no_work(
                "observer-a",
                observer_a_count.clone(),
            )),
            Box::new(CountingStrand::no_work(
                "observer-b",
                observer_b_count.clone(),
            )),
        ]);
        let store = EmptyStore;
        let exclusions = HashSet::new();
        let outcome = runner.select(&store, &exclusions).await.unwrap();
        assert!(outcome.bead.is_none());

        // creator should have been evaluated MAX_RESTARTS + 1 = 4 times.
        assert_eq!(creator_count.load(std::sync::atomic::Ordering::SeqCst), 4);

        // After the restart cap, the remaining strands should each be evaluated
        // at least once (the final pass through the waterfall).
        let a = observer_a_count.load(std::sync::atomic::Ordering::SeqCst);
        let b = observer_b_count.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            a >= 1,
            "observer_a should be evaluated at least once, got {a}"
        );
        assert!(
            b >= 1,
            "observer_b should be evaluated at least once, got {b}"
        );

        // All 4 WorkCreated events are captured in restart_triggers.
        assert_eq!(outcome.waterfall_restarts, 4);
        assert_eq!(outcome.restart_triggers.len(), 4);
        assert!(outcome.restart_triggers.iter().all(|t| t == "creator"));
    }

    #[tokio::test]
    async fn empty_bead_found_continues_to_next() {
        let bead = make_test_bead("real-bead");
        let runner = StrandRunner::new(vec![
            Box::new(StubStrand::beads("empty-finder", vec![])),
            Box::new(StubStrand::beads("real-finder", vec![bead])),
        ]);
        let store = EmptyStore;
        let exclusions = HashSet::new();
        let outcome = runner.select(&store, &exclusions).await.unwrap();
        assert_eq!(
            outcome.bead.map(|(b, _)| b.id),
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
        let exclusions = HashSet::new();
        let outcome = runner.select(&store, &exclusions).await.unwrap();
        assert_eq!(
            outcome.bead.map(|(b, _)| b.id),
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
            vec![
                "pluck", "mend", "explore", "weave", "unravel", "pulse", "reflect", "splice",
                "knot"
            ]
        );
    }
}
