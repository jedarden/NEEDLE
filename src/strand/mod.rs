//! Strand waterfall: ordered sequence of selection strategies.
//!
//! The StrandRunner evaluates strands in priority order. The first strand
//! that yields a candidate wins. Strands are stateless — they receive queue
//! state and return a candidate or nothing.
//!
//! Depends on: `types`, `config`, `bead_store`.

pub mod analyze;
pub mod ci_watch;
mod explore;
mod generation;
mod knot;
pub mod mend;
mod pluck;
pub mod pulse;
pub mod reflect;
pub mod splice;
pub mod unravel;
pub mod weave;
pub mod weft;
pub mod weft_client;
pub mod weft_envelope;
pub mod weft_executor;
pub(crate) mod workspace_capacity;
mod workspace_health;

use std::collections::HashSet;
use std::time::Instant;

use anyhow::Result;
use tracing::Instrument;

use crate::bead_store::BeadStore;
use crate::config::Config;
use crate::span::{attrs, strand_results};
use crate::telemetry::CycleOutcome;
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
    /// If set, this bead should be dispatched with a split prompt instead of
    /// the normal work prompt. Contains the consecutive failure count.
    pub split_failure_count: Option<u32>,
    /// External work completed by a strand without a local bead.
    pub work_performed: Option<(String, serde_json::Value)>,
}

pub use analyze::{AnalysisAgent, AnalyzeStrand};
pub use ci_watch::CiWatchStrand;
pub use explore::ExploreStrand;
pub use knot::KnotStrand;
pub use mend::{cleanup_orphaned_in_progress, MendStrand};
pub use pluck::PluckStrand;
pub use pulse::PulseStrand;
pub use reflect::{CliReflectAgent, ReflectAgent, ReflectStrand};
pub use splice::SpliceStrand;
pub use unravel::{UnravelAgent, UnravelStrand};
pub use weave::{CliWeaveAgent, FleetWeaveStrand, WeaveAgent, WeaveStrand};
pub use weft::WeftStrand;

/// A single selection strategy in the waterfall.
#[async_trait::async_trait]
pub trait Strand: Send + Sync {
    /// Human-readable name for telemetry.
    fn name(&self) -> &str;

    /// Evaluate this strand against the current queue state.
    async fn evaluate(&self, store: &dyn BeadStore, exclusions: &HashSet<BeadId>) -> StrandResult;

    /// Break dependency cycles detected in the bead store.
    ///
    /// Default implementation does nothing. Strands can override this to provide
    /// cycle-breaking logic.
    async fn break_dependency_cycles(&self, _store: &dyn BeadStore) -> anyhow::Result<()> {
        Ok(())
    }

    /// Append notes to a bead.
    ///
    /// Default implementation does nothing. Strands can override this to provide
    /// note-adding logic.
    async fn append_bead_notes(
        &self,
        _store: &dyn BeadStore,
        _bead_id: &BeadId,
        _note: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Whether this strand manufactures new work rather than selecting work
    /// that already exists.
    ///
    /// The runner uses this to tell a recoverable generator failure apart from
    /// an ordinary selection error: a failed generator falls through to the
    /// next generator, and that fall-through is recorded as `creator_failed`
    /// rather than as idle.
    fn is_generator(&self) -> bool {
        false
    }
}

/// Runs strands in order, returning the first candidate found.
pub struct StrandRunner {
    strands: Vec<Box<dyn Strand>>,
    telemetry: crate::telemetry::Telemetry,
    /// Shared with Knot so that a cycle records at most one outcome: Knot is
    /// the terminal classifier, but a cycle that already selected or generated
    /// work must not also be counted as idle or starvation.
    cycle_outcome_recorded: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl StrandRunner {
    pub fn new(strands: Vec<Box<dyn Strand>>) -> Self {
        Self::with_telemetry(
            strands,
            crate::telemetry::Telemetry::new("strand-runner".to_string()),
        )
    }

    /// Construct a runner with an explicit telemetry sink.
    ///
    /// Production uses [`Self::new`].  The injectable sink keeps embedded
    /// runners and integration fixtures from writing their cycle evidence to
    /// the process-wide default state directory.
    pub fn with_telemetry(
        strands: Vec<Box<dyn Strand>>,
        telemetry: crate::telemetry::Telemetry,
    ) -> Self {
        StrandRunner {
            strands,
            telemetry,
            cycle_outcome_recorded: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Build the default strand waterfall from config.
    ///
    /// The waterfall order is:
    /// Pluck → Mend → Explore → Weave → Unravel → Analyze → Pulse → Reflect → Splice → Knot.
    pub fn from_config(
        config: &Config,
        worker_id: &str,
        registry: crate::registry::Registry,
        telemetry: crate::telemetry::Telemetry,
    ) -> Self {
        telemetry.configure_explore_starvation(config.strands.explore.starvation_threshold_minutes);
        // StrandRunner is also constructed directly by embedders and tests;
        // publish the config value before any strand resolves a state path.
        crate::state_dir::set_configured(config.paths.state_dir.clone());
        // A reserved lane (`strands.pluck.lanes`) binds this worker to one
        // label for the whole waterfall: Pluck's selection at home and
        // Explore's admission abroad read the same binding, so a lane worker
        // cannot be handed unlabelled work by the strand the other one does
        // not cover. No lane matches this worker => `None` => no behaviour
        // change anywhere.
        let lane = config.strands.pluck.lane_for(worker_id).cloned();
        let circuit_checker = config
            .strands
            .pluck
            .circuit_breaker
            .enabled
            .then(crate::build_status::BuildStatusChecker::production);

        // All host-level strand state follows the single configured root.
        // `root_for` retains the historical workspace-home fallback when no
        // central override is active, keeping direct in-process callers
        // compatible while making NEEDLE_STATE_DIR/paths.state_dir atomic.
        let state_root = crate::state_dir::root_for(&config.workspace.home);
        let state_base = state_root.join("state");
        let heartbeat_dir = state_base.join("heartbeats");
        let heartbeat_ttl = std::time::Duration::from_secs(config.health.heartbeat_ttl_secs);
        let ci_watch = CiWatchStrand::new(
            config.strands.ci_watch.clone(),
            config.workspace.default.clone(),
            state_base.join("ci_watch"),
            telemetry.clone(),
        );

        let pluck = PluckStrand::with_persistent_records(
            config.strands.pluck.exclude_labels.clone(),
            config.strands.pluck.split_after_failures,
            telemetry.clone(),
            state_root.clone(),
            config.strands.pluck.persistent_starvation_records,
        )
        .with_quarantine_threshold(config.outcome.quarantine_after_failures)
        .with_lane(lane.clone())
        .with_workspace_capacity(
            config.workspace.default.clone(),
            heartbeat_dir.clone(),
            heartbeat_ttl,
        );
        let pluck = if let Some(checker) = &circuit_checker {
            pluck.with_circuit_breaker(
                config.workspace.default.clone(),
                checker.clone(),
                config.strands.pluck.circuit_breaker.labels.clone(),
            )
        } else {
            pluck
        };

        let lock_dir = std::env::temp_dir();
        let log_dir = crate::state_dir::logs_dir_for(
            config.telemetry.file_sink.log_dir.as_deref(),
            &state_root,
        );
        let retention_days = config.telemetry.file_sink.retention_days;
        let traces_dir = config.workspace.default.join(".beads").join("traces");
        let trace_retention_failed_days = config.strands.learning.trace_retention_failed_days;
        let trace_retention_success_days = config.strands.learning.trace_retention_success_days;

        // Create a new Registry instance pointing to the same path for ExploreStrand.
        // We need to get the state_dir_for_explore before moving registry to MendStrand.
        let state_fallback = state_base.clone();
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
            state_base.clone(),
            config.limits.clone(),
        )
        .with_attempt_archive_retention(
            config.attempt_archive.enabled,
            config.attempt_archive.prune_local_after_spool,
        );

        let explore = ExploreStrand::new(
            config.strands.explore.clone(),
            config.workspace.default.clone(),
            explore_registry,
            telemetry.clone(),
            worker_id.to_string(),
        )
        .with_heartbeat_ttl(heartbeat_ttl)
        // Explore must reject beads the worker would immediately release as
        // split_out_of_scope; without the threshold it cannot tell which those
        // are (needle-ee024ae4).
        .with_split_after_failures(config.strands.pluck.split_after_failures)
        .with_lane(lane);
        let explore = if let Some(checker) = circuit_checker {
            explore
                .with_circuit_breaker(checker, config.strands.pluck.circuit_breaker.labels.clone())
        } else {
            explore
        };

        // Rung 4 of the escalation ladder (Phase 19 §19.2, ADR-022 decision 3):
        // settles a bead whose round-3 quarantine expired — the bead Pluck
        // parked rather than re-dispatched. It sits after Unravel, whose prompt
        // machinery it reuses and to which it hands rung 5 (the `human` label)
        // back, and ahead of the observation strands so a concluded re-scope
        // returns `WorkCreated` and restarts the waterfall from Pluck, making
        // the fresh child claimable the same cycle.
        let analyze = AnalyzeStrand::new(
            config.strands.analyze.clone(),
            config.workspace.default.clone(),
            state_base.join("analyze"),
            Box::new(analyze::CliAnalysisAgent::new(config.agent.default.clone())),
            telemetry.clone(),
        );

        let weave_agent = match CliWeaveAgent::from_config(config) {
            Ok(agent) => agent,
            Err(error) => {
                tracing::warn!(
                    adapter = %config.agent.default,
                    error = %error,
                    "failed to resolve Weave adapter; generation will report creator failure"
                );
                CliWeaveAgent::unavailable(error.to_string())
            }
        };
        let weave: Box<dyn Strand> = if config.strands.explore.enabled {
            Box::new(FleetWeaveStrand::new(
                config.strands.weave.clone(),
                config.strands.explore.clone(),
                state_base.join("weave"),
                Box::new(weave_agent),
                telemetry.clone(),
                config.strands.generation.clone(),
                config.strands.pluck.exclude_labels.clone(),
                worker_id.to_string(),
            ))
        } else {
            Box::new(
                WeaveStrand::new(
                    config.strands.weave.clone(),
                    config.workspace.default.clone(),
                    state_base.join("weave"),
                    Box::new(weave_agent),
                    telemetry.clone(),
                )
                .with_generation(
                    config.strands.generation.clone(),
                    config.strands.pluck.exclude_labels.clone(),
                ),
            )
        };

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
        )
        .with_generation(
            config.strands.generation.clone(),
            config.strands.pluck.exclude_labels.clone(),
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
        let splice_heartbeat_dir = state_base.join("heartbeats");
        let runner_telemetry = telemetry.clone();

        // VALIDATION: Warn loudly if Splice is enabled but report_workspace is unset.
        // This is a critical configuration gap - without report_workspace, Splice will
        // silently fail to create escalation beads, missing the entire point of detection.
        if config.strands.splice.enabled && config.strands.splice.report_workspace.is_none() {
            tracing::warn!(
                "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\
                 \n\
                 Splice strand is ENABLED but strands.splice.report_workspace is NOT SET!\
                 \n\
                 Worker failure and live-loop detection will NOT create escalation beads.\
                 \n\
                 To fix, add to your ~/.config/needle/config.yaml:\
                 \n\
                   strands:\
                     splice:\
                       report_workspace: /path/to/your/workspace\
                 \n\
                 Or set NEEDLE_STRANDS__SPLICE__REPORT_WORKSPACE=/path/to/workspace\
                 \n\
                 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
            );
        }

        let splice = SpliceStrand::new(
            config.strands.splice.clone(),
            splice_heartbeat_dir,
            state_base.join("splice"),
            telemetry,
        );

        let knot = KnotStrand::with_workspace(
            config.strands.knot.clone(),
            runner_telemetry.clone(),
            config.workspace.default.clone(),
        );
        let cycle_outcome_recorded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let weft_enabled = config.loom.enabled && !config.loom.serve_strands.is_empty();
        let make_weft = || -> Box<dyn Strand> {
            Box::new(WeftStrand::new(
                config.loom.clone(),
                if config.loom.worker_name.is_empty() {
                    worker_id.to_string()
                } else {
                    config.loom.worker_name.clone()
                },
                std::sync::Arc::new(weft_executor::MappedWeftExecutor::new(
                    config.clone(),
                    runner_telemetry.clone(),
                )),
                runner_telemetry.clone(),
            ))
        };
        let mut strands: Vec<Box<dyn Strand>> = Vec::with_capacity(12);
        if weft_enabled && config.loom.position == crate::config::LoomPosition::BeforePluck {
            strands.push(make_weft());
        }
        if config.strands.ci_watch.enabled {
            strands.push(Box::new(ci_watch));
        }
        strands.push(Box::new(pluck));
        if weft_enabled && config.loom.position == crate::config::LoomPosition::AfterPluck {
            strands.push(make_weft());
        }
        strands.extend(vec![
            Box::new(mend) as Box<dyn Strand>,
            Box::new(explore),
            weave,
            Box::new(unravel),
            Box::new(analyze),
            Box::new(pulse),
            Box::new(reflect),
            Box::new(splice),
            Box::new(knot.with_cycle_outcome_guard(cycle_outcome_recorded.clone())),
        ]);
        StrandRunner {
            strands,
            telemetry: runner_telemetry,
            cycle_outcome_recorded,
        }
    }

    /// Run the CI observer independently of execution admission.
    ///
    /// CI polling is deliberately a control-plane operation: it performs a
    /// bounded status read and may create a repair bead, but it never claims
    /// or dispatches work. Keeping it ahead of the worker's CPU/memory hold
    /// preserves the configured wall-clock poll interval on a saturated host.
    /// The ordinary waterfall evaluates it again after admission; its own
    /// interval gate makes that second evaluation a no-op.
    pub async fn observe_ci_before_admission(&self, store: &dyn BeadStore) -> StrandResult {
        let Some(strand) = self
            .strands
            .iter()
            .find(|strand| strand.name() == "ci_watch")
        else {
            return StrandResult::NoWork;
        };

        let strand_span = tracing::info_span!(
            "strand.ci_watch",
            needle.strand.name = "ci_watch",
            needle.strand.phase = "pre_admission",
        );
        let start = Instant::now();
        let result = strand
            .evaluate(store, &HashSet::new())
            .instrument(strand_span)
            .await;
        let result_name = match &result {
            StrandResult::WorkCreated => strand_results::WORK_CREATED,
            StrandResult::NoWork => strand_results::NO_WORK,
            StrandResult::Error(_) => strand_results::ERROR,
            StrandResult::BeadFound(_) | StrandResult::Split(_, _) => strand_results::BEAD_FOUND,
            StrandResult::WorkPerformed { .. } => "work_performed",
            StrandResult::Skipped { .. } => "skipped",
            StrandResult::FoundButExcluded => "found_but_excluded",
        };
        if let Err(error) = self.telemetry.emit(
            crate::telemetry::EventKind::StrandEvaluated {
                strand_name: "ci_watch".to_string(),
                result: result_name.to_string(),
                duration_ms: start.elapsed().as_millis() as u64,
            },
            chrono::Utc::now(),
        ) {
            tracing::warn!(%error, "failed to emit pre-admission ci-watch evaluation");
        }
        result
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
        self.cycle_outcome_recorded
            .store(false, std::sync::atomic::Ordering::SeqCst);
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
                let start = Instant::now();
                let result = strand
                    .evaluate(store, exclusions)
                    .instrument(strand_span.clone())
                    .await;
                let elapsed_ms = start.elapsed().as_millis() as u64;

                // The remaining evaluation bookkeeping is synchronous, so a
                // scoped guard is safe here and preserves strand context on its
                // tracing events without crossing an `.await`.
                let _strand_enter = strand_span.enter();

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
                    StrandResult::WorkPerformed { .. } => ("work_performed".to_string(), true),
                    StrandResult::NoWork => (strand_results::NO_WORK.to_string(), true),
                    StrandResult::Error(_) => (strand_results::ERROR.to_string(), true),
                    StrandResult::Skipped { reason } => (format!("skipped({})", reason), true),
                    StrandResult::Split(_, _failure_count) => {
                        (format!("{}({})", strand_results::BEAD_FOUND, 1), true)
                    }
                    StrandResult::FoundButExcluded => ("found_but_excluded".to_string(), true),
                };
                tracing::Span::current().record(attrs::NEEDLE_STRAND_RESULT, &result_str);
                tracing::Span::current().record(attrs::NEEDLE_STRAND_DURATION_MS, elapsed_ms);

                // Set strand span status: Error for strand errors, Ok for all other results
                // Note: Skipped is not an error - it's expected for roam-only workers
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

                        if let Err(e) = self.telemetry.emit(
                            crate::telemetry::EventKind::StrandEvaluated {
                                strand_name: strand_name.clone(),
                                result: "bead_found".to_string(),
                                duration_ms: elapsed_ms,
                            },
                            chrono::Utc::now(),
                        ) {
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
                            self.emit_cycle_outcome(
                                CycleOutcome::Selected,
                                Some(strand_name.clone()),
                                None,
                            );
                            return Ok(SelectOutcome {
                                bead: Some((bead, strand_name.clone())),
                                waterfall_restarts: restarts,
                                restart_triggers,
                                strand_evaluations,
                                split_failure_count: None,
                                work_performed: None,
                            });
                        }
                        continue;
                    }
                    StrandResult::Split(bead, failure_count) => {
                        let bead = *bead;
                        if let Err(e) = self.telemetry.emit(
                            crate::telemetry::EventKind::StrandEvaluated {
                                strand_name: strand_name.clone(),
                                result: "split".to_string(),
                                duration_ms: elapsed_ms,
                            },
                            chrono::Utc::now(),
                        ) {
                            tracing::warn!(
                                strand = %strand_name,
                                error = %e,
                                "failed to emit strand evaluated telemetry"
                            );
                        }
                        tracing::info!(
                            strand = %strand_name,
                            bead_id = %bead.id,
                            failure_count,
                            "strand triggered split for bead with excessive failures"
                        );
                        return Ok(SelectOutcome {
                            bead: Some((bead, strand_name.clone())),
                            waterfall_restarts: restarts,
                            restart_triggers,
                            strand_evaluations,
                            split_failure_count: Some(failure_count),
                            work_performed: None,
                        });
                    }
                    StrandResult::WorkPerformed { summary, telemetry } => {
                        if let Err(error) = self.telemetry.emit(
                            crate::telemetry::EventKind::StrandEvaluated {
                                strand_name: strand_name.clone(),
                                result: "work_performed".to_string(),
                                duration_ms: elapsed_ms,
                            },
                            chrono::Utc::now(),
                        ) {
                            tracing::warn!(strand = %strand_name, error = %error, "failed to emit strand evaluation telemetry");
                        }
                        self.emit_cycle_outcome(
                            CycleOutcome::WorkPerformed,
                            Some(strand_name.clone()),
                            Some(summary.clone()),
                        );
                        return Ok(SelectOutcome {
                            bead: None,
                            waterfall_restarts: restarts,
                            restart_triggers,
                            strand_evaluations,
                            split_failure_count: None,
                            work_performed: Some((summary, telemetry)),
                        });
                    }
                    StrandResult::WorkCreated => {
                        if let Err(e) = self.telemetry.emit(
                            crate::telemetry::EventKind::StrandEvaluated {
                                strand_name: strand_name.clone(),
                                result: "work_created".to_string(),
                                duration_ms: elapsed_ms,
                            },
                            chrono::Utc::now(),
                        ) {
                            tracing::warn!(
                                strand = %strand_name,
                                error = %e,
                                "failed to emit strand evaluated telemetry"
                            );
                        }
                        // Real work was manufactured rather than selected, so
                        // this is a generation cycle even if the restart below
                        // never manages to claim the new bead.
                        self.emit_cycle_outcome(
                            CycleOutcome::Generated,
                            Some(strand_name.clone()),
                            None,
                        );
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
                        if let Err(e) = self.telemetry.emit(
                            crate::telemetry::EventKind::StrandEvaluated {
                                strand_name: strand_name.clone(),
                                result: "no_work".to_string(),
                                duration_ms: elapsed_ms,
                            },
                            chrono::Utc::now(),
                        ) {
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
                    StrandResult::Skipped { reason } => {
                        if let Err(e) = self.telemetry.emit(
                            crate::telemetry::EventKind::StrandEvaluated {
                                strand_name: strand_name.clone(),
                                result: format!("skipped({})", reason),
                                duration_ms: elapsed_ms,
                            },
                            chrono::Utc::now(),
                        ) {
                            tracing::warn!(
                                strand = %strand_name,
                                error = %e,
                                "failed to emit strand evaluated telemetry"
                            );
                        }
                        tracing::info!(
                            strand = %strand_name,
                            reason = %reason,
                            elapsed_ms,
                            "strand skipped"
                        );
                        continue;
                    }
                    StrandResult::FoundButExcluded => {
                        if let Err(e) = self.telemetry.emit(
                            crate::telemetry::EventKind::StrandEvaluated {
                                strand_name: strand_name.clone(),
                                result: "found_but_excluded".to_string(),
                                duration_ms: elapsed_ms,
                            },
                            chrono::Utc::now(),
                        ) {
                            tracing::warn!(
                                strand = %strand_name,
                                error = %e,
                                "failed to emit strand evaluated telemetry"
                            );
                        }
                        tracing::info!(
                            strand = %strand_name,
                            elapsed_ms,
                            "strand found candidates but all were excluded/assigned, triggering short retry"
                        );
                        continue;
                    }
                    StrandResult::Error(e) => {
                        if let Err(te) = self.telemetry.emit(
                            crate::telemetry::EventKind::StrandEvaluated {
                                strand_name: strand_name.clone(),
                                result: "error".to_string(),
                                duration_ms: elapsed_ms,
                            },
                            chrono::Utc::now(),
                        ) {
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
                        // A generator that fails recoverably is a distinct
                        // outcome from idle: the cycle did try to manufacture
                        // work and will fall through to the next generator.
                        // The generator itself records `creator_failed` detail.
                        if strand.is_generator() {
                            self.emit_cycle_outcome(
                                CycleOutcome::CreatorFailed,
                                Some(strand_name.clone()),
                                None,
                            );
                        }
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
                split_failure_count: None,
                work_performed: None,
            });
        }
    }

    /// Return the names of all configured strands (for telemetry/debugging).
    pub fn strand_names(&self) -> Vec<&str> {
        self.strands.iter().map(|s| s.name()).collect()
    }

    /// Record how a selection cycle resolved.
    ///
    /// Exactly one outcome is recorded per cycle: the first of a selection, a
    /// generation restart, or a generator failure wins. Terminal idle and
    /// terminal starvation are recorded by Knot, which is the last strand in
    /// an exhausted cycle and the only place that can tell the two apart.
    fn emit_cycle_outcome(
        &self,
        outcome: CycleOutcome,
        strand_name: Option<String>,
        detail: Option<String>,
    ) {
        if self
            .cycle_outcome_recorded
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            tracing::debug!(
                outcome = outcome.as_str(),
                "cycle outcome already recorded; not overwriting it"
            );
            return;
        }
        if let Some(detail) = detail {
            tracing::info!(outcome = outcome.as_str(), detail = %detail, "cycle outcome");
        }
        let _ = self.telemetry.emit(
            crate::telemetry::EventKind::CycleOutcome {
                outcome,
                workspace: None,
                strand_name,
            },
            chrono::Utc::now(),
        );
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::fixture_root;
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

        async fn evaluate(
            &self,
            _store: &dyn BeadStore,
            _exclusions: &HashSet<BeadId>,
        ) -> StrandResult {
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
            workspace: fixture_root("test"),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
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
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn claim_auto(&self, _actor: &str) -> Result<crate::types::ClaimResult> {
            Ok(crate::types::ClaimResult::NotClaimable {
                reason: "claim_auto not supported in mock".to_string(),
            })
        }

        fn has_valid_store(&self) -> bool {
            true // Mock store always has a valid store
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
        let terminal_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let runner = StrandRunner::new(vec![
            Box::new(StubStrand::work_created("creator")),
            Box::new(StubStrand::beads(
                "finder",
                vec![make_test_bead("test-002")],
            )),
            Box::new(CountingStrand::no_work("knot", terminal_count.clone())),
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
        assert_eq!(
            terminal_count.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "terminal Knot must not diagnose starvation after an earlier strand creates selectable work"
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
        let outcome = runner.select(&store, &HashSet::new()).await.unwrap();
        assert!(outcome.bead.is_none());
        assert_eq!(outcome.waterfall_restarts, 0);
    }

    #[tokio::test]
    async fn strand_names_and_pre_admission_observer_are_scoped() {
        let ci_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let pluck_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let runner = StrandRunner::new(vec![
            Box::new(CountingStrand::work_created("ci_watch", ci_count.clone())),
            Box::new(CountingStrand::no_work("pluck", pluck_count.clone())),
        ]);

        assert_eq!(runner.strand_names(), vec!["ci_watch", "pluck"]);

        let result = runner.observe_ci_before_admission(&EmptyStore).await;

        assert!(matches!(result, StrandResult::WorkCreated));
        assert_eq!(ci_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(pluck_count.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// A strand that always returns WorkCreated (never consumed).
    struct AlwaysWorkCreated;

    #[async_trait::async_trait]
    impl Strand for AlwaysWorkCreated {
        fn name(&self) -> &str {
            "always-creates"
        }
        async fn evaluate(
            &self,
            _store: &dyn BeadStore,
            _exclusions: &HashSet<BeadId>,
        ) -> StrandResult {
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

        async fn evaluate(
            &self,
            _store: &dyn BeadStore,
            _exclusions: &HashSet<BeadId>,
        ) -> StrandResult {
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

    struct ConformanceExecutor {
        harness: crate::strand::weft_envelope::WeftHarness,
    }

    #[async_trait::async_trait]
    impl crate::strand::weft::WeftTurnExecutor for ConformanceExecutor {
        async fn execute(
            &self,
            brief: crate::strand::weft_client::LoomBrief,
            _turn_id: String,
            _client: std::sync::Arc<crate::strand::weft_client::LoomClient>,
        ) -> Result<crate::strand::weft::WeftExecution, crate::strand::weft_client::LoomError>
        {
            let request = crate::strand::weft_envelope::EnvelopeRequest {
                capabilities: brief.envelope["capabilities"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap().to_string())
                    .collect(),
                deny_paths: brief.envelope["deny_paths"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap().to_string())
                    .collect(),
            };
            crate::strand::weft_envelope::translate(self.harness, &request)
                .expect("recorded translation fixture must be expressible for this mapping");
            tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
            let completion = serde_json::json!({
                "contract_version": 1,
                "parse_fallback": false,
                "reply_markdown": "conformance reply"
            });
            Ok(crate::strand::weft::WeftExecution {
                summary: "conformance reply".to_string(),
                telemetry: serde_json::json!({"conformance": true}),
                completion,
            })
        }
    }

    fn serve_conformance_flow(
        brief: serde_json::Value,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let turn = serde_json::json!([{
            "id": "turn-conformance",
            "strand": "advisor",
            "target_kind": "post",
            "target_id": "post-1",
            "queued_at": "2026-10-08T00:00:00Z",
            "attempt": 1,
            "contract_version": 1
        }])
        .to_string();
        let claim =
            r#"{"id":"turn-conformance","lease_expires_at":"2026-10-08T00:02:00Z","attempt":1}"#
                .to_string();
        let heartbeat = r#"{"lease_expires_at":"2026-10-08T00:02:00Z"}"#.to_string();
        let complete = "{}".to_string();
        let bodies = vec![turn, claim, brief.to_string(), heartbeat, complete];
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::with_capacity(bodies.len());
            for body in bodies {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "mock LOOM server timed out"
                            );
                            std::thread::yield_now();
                        }
                        Err(error) => panic!("mock LOOM accept failed: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 2048];
                loop {
                    let size = stream.read(&mut chunk).unwrap();
                    if size == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..size]);
                    let Some(header_end) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|value| value.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&request).to_string());
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ).unwrap();
            }
            requests
        });
        (url, handle)
    }

    fn verify_translation_fixture_table(
        fixture: &serde_json::Value,
        harness: crate::strand::weft_envelope::WeftHarness,
        expected_key: &str,
    ) {
        use crate::strand::weft_envelope::{translate, EnvelopeRequest};
        assert_eq!(fixture["schema"], "loom.conformance.translation.v1");
        assert_eq!(fixture["adapter"], expected_key.replace('_', "-"));
        let cases = fixture["cases"].as_array().expect("fixture cases");
        assert_eq!(cases.len(), 14);
        for case in cases {
            let input = &case["input"];
            let request = EnvelopeRequest {
                capabilities: input["capabilities"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap().to_string())
                    .collect(),
                deny_paths: input["deny_paths"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap().to_string())
                    .collect(),
            };
            let expected = &case["expected"];
            if expected["outcome"] == "translated" {
                let actual = translate(harness, &request).unwrap_or_else(|error| {
                    panic!("{} unexpectedly rejected: {error}", case["case_id"])
                });
                let table = &expected[expected_key];
                let string_list = |value: &serde_json::Value| {
                    value
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|item| item.as_str().unwrap().to_string())
                        .collect::<Vec<_>>()
                };
                if harness == crate::strand::weft_envelope::WeftHarness::ClaudePrint {
                    assert_eq!(
                        actual.allowed_tools,
                        string_list(&table["allowed_tools"]),
                        "{}",
                        case["case_id"]
                    );
                    assert_eq!(
                        actual.disallowed_tools,
                        string_list(&table["disallowed_tools"]),
                        "{}",
                        case["case_id"]
                    );
                }
                if harness == crate::strand::weft_envelope::WeftHarness::Codex {
                    assert!(actual.allowed_tools.is_empty());
                    assert!(actual.disallowed_tools.is_empty());
                    assert_eq!(
                        actual.sandbox_mode.as_deref(),
                        table["sandbox_mode"].as_str()
                    );
                    assert_eq!(
                        actual.approval_policy.as_deref(),
                        table["approval_policy"].as_str()
                    );
                    assert_eq!(actual.writable_roots, string_list(&table["writable_roots"]));
                    assert_eq!(
                        actual.network_access,
                        table["network_access"].as_bool().unwrap()
                    );
                }
            } else {
                let error =
                    translate(harness, &request).expect_err("unsupported fixture must fail closed");
                assert_eq!(
                    error.error_code,
                    expected["fail"]["error_code"].as_str().unwrap()
                );
                assert!(!error.retryable);
                assert_eq!(error.capability, expected["subject"].as_str().unwrap());
            }
        }
    }

    fn request_line(request: &str) -> &str {
        request.lines().next().expect("HTTP request line")
    }

    fn request_body(request: &str) -> serde_json::Value {
        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap()
    }

    #[tokio::test]
    async fn loom_translation_fixtures_drive_mock_worker_round_trips() {
        use crate::config::{LoomConfig, LoomServeStrand};
        use crate::strand::weft::WeftStrand;
        use crate::strand::weft_envelope::WeftHarness;
        use crate::telemetry::Telemetry;
        use std::os::unix::fs::PermissionsExt;

        let fixture_dir = std::env::var_os("NEEDLE_LOOM_CONFORMANCE_DIR")
            .map(std::path::PathBuf::from)
            .expect("CI/local conformance replay requires NEEDLE_LOOM_CONFORMANCE_DIR");
        let claude: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixture_dir.join("claude_print.json")).unwrap())
                .unwrap();
        let codex: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixture_dir.join("codex.json")).unwrap())
                .unwrap();
        let claude_cases = claude["cases"].as_array().unwrap();
        let codex_cases = codex["cases"].as_array().unwrap();
        assert_eq!(claude_cases.len(), codex_cases.len());
        for (claude_case, codex_case) in claude_cases.iter().zip(codex_cases) {
            assert_eq!(claude_case["case_id"], codex_case["case_id"]);
            assert_eq!(claude_case["input"], codex_case["input"]);
        }
        verify_translation_fixture_table(&claude, WeftHarness::ClaudePrint, "claude_print");
        verify_translation_fixture_table(&codex, WeftHarness::Codex, "codex");

        for (adapter, harness, fixture, case_id) in [
            (
                "claude-print",
                WeftHarness::ClaudePrint,
                &claude,
                "envelope.advisor",
            ),
            ("codex", WeftHarness::Codex, &codex, "capability.read"),
        ] {
            let case = fixture["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["case_id"] == case_id)
                .unwrap();
            let selected = &case["input"];
            let brief = serde_json::json!({
                "contract_version": 1,
                "turn": {"strand":"advisor"},
                "envelope": selected,
                "prompt_markdown": "fixture prompt"
            });
            let (url, server) = serve_conformance_flow(brief);
            let directory = tempfile::tempdir().unwrap();
            let token_file = directory.path().join("loom.token");
            std::fs::write(&token_file, "conformance-test-token").unwrap();
            std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600)).unwrap();
            let mut config = LoomConfig {
                enabled: true,
                base_url: url,
                token_file,
                worker_name: "conformance-worker".to_string(),
                lease_secs: 7,
                heartbeat_secs: 1,
                ..LoomConfig::default()
            };
            config.serve_strands.insert(
                "advisor".to_string(),
                LoomServeStrand {
                    adapter: adapter.to_string(),
                    model: Some("conformance-model".to_string()),
                },
            );
            let strand = WeftStrand::new(
                config,
                "conformance-worker",
                std::sync::Arc::new(ConformanceExecutor { harness }),
                Telemetry::new("weft-conformance".to_string()),
            );
            let result = strand.evaluate(&EmptyStore, &HashSet::new()).await;
            assert!(matches!(result, StrandResult::WorkPerformed { .. }));
            let requests = server.join().unwrap();
            assert_eq!(requests.len(), 5);
            assert_eq!(
                requests
                    .iter()
                    .map(|request| request_line(request))
                    .collect::<Vec<_>>(),
                vec![
                    "GET /api/v1/turns?status=queued&strand=advisor&limit=10 HTTP/1.1",
                    "POST /api/v1/turns/turn-conformance/claim HTTP/1.1",
                    "GET /api/v1/turns/turn-conformance/brief HTTP/1.1",
                    "POST /api/v1/turns/turn-conformance/heartbeat HTTP/1.1",
                    "POST /api/v1/turns/turn-conformance/complete HTTP/1.1",
                ]
            );
            for request in &requests {
                let lower = request.to_ascii_lowercase();
                assert!(lower.contains("authorization: bearer conformance-test-token"));
                assert!(lower.contains("x-loom-worker: conformance-worker"));
            }
            assert_eq!(
                request_body(&requests[1]),
                serde_json::json!({"lease_secs":7})
            );
            assert_eq!(request_body(&requests[3]), serde_json::json!({}));
            assert_eq!(
                request_body(&requests[4]),
                serde_json::json!({
                    "contract_version":1,
                    "parse_fallback":false,
                    "reply_markdown":"conformance reply"
                })
            );
        }
    }

    #[tokio::test]
    async fn from_config_places_weft_before_pluck_or_between_pluck_and_mend() {
        use crate::config::{LoomPosition, LoomServeStrand};

        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::isolated_for_test();
        config.loom.enabled = true;
        config.loom.serve_strands.insert(
            "advisor".to_string(),
            LoomServeStrand {
                adapter: "claude-print".to_string(),
                model: Some("claude-sonnet-5".to_string()),
            },
        );
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());

        config.loom.position = LoomPosition::BeforePluck;
        let before = StrandRunner::from_config(
            &config,
            "test-worker",
            crate::registry::Registry::new(dir.path()),
            telemetry.clone(),
        );
        assert_eq!(before.strand_names()[..3], ["weft", "pluck", "mend"]);

        config.loom.position = LoomPosition::AfterPluck;
        let after = StrandRunner::from_config(
            &config,
            "test-worker",
            crate::registry::Registry::new(dir.path()),
            telemetry,
        );
        assert_eq!(after.strand_names()[..3], ["pluck", "weft", "mend"]);
    }

    #[tokio::test]
    async fn from_config_includes_full_waterfall() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::isolated_for_test();
        let registry = crate::registry::Registry::new(dir.path());
        let telemetry = crate::telemetry::Telemetry::new("test".to_string());
        let runner = StrandRunner::from_config(&config, "test-worker", registry, telemetry);
        // `analyze` must actually be IN the waterfall: the strand once existed
        // — logic, prompt, tests, everything — while `from_config` constructed
        // it into `let _analyze` and dropped it, leaving every expired round-3
        // bead parked forever with nothing to settle it.
        assert_eq!(
            runner.strand_names(),
            vec![
                "pluck", "mend", "explore", "weave", "unravel", "analyze", "pulse", "reflect",
                "splice", "knot"
            ]
        );
    }
}
