//! Controller-owned application of validated Resolve decisions to bead
//! lifecycle (plan.md HANDLING "Resolve decisions", N-T04's guarded
//! application half).
//!
//! The resolver classifies evidence and produces a [`ResolveDecision`]; this
//! module is the only place a decision becomes bead mutations. Every action
//! follows the same discipline:
//!
//! - **Ownership is re-checked before every mutation.** A dispatch that lost
//!   its claim while resolving — another worker reaped or re-claimed the
//!   bead, the agent closed it late — must not alter a bead that has since
//!   changed hands. The check is [`BeadStore::claim_status`] (live store:
//!   `in_progress` + assigned to this worker), the same predicate
//!   `verify_claim_at_dispatch` uses.
//! - **A failed mutation releases safely.** The post-dispatch invariant
//!   (plan.md HANDLING) says a dispatch may never leave its bead
//!   `in_progress`: whenever a mutation fails mid-application, the executor
//!   attempts an ownership-checked release of its own and reports the
//!   outcome. A decision that was refused before any mutation (invalid
//!   proposal, wrong parent) gets the same best-effort release, per the
//!   documented `resolution_failed` fallback.
//! - **Gates and shipped-work verification gate `complete`.** The closure is
//!   judged by the bead workspace's configured gates (the same per-workspace
//!   resolution the dispatch path uses, via
//!   [`OutcomeHandler::run_verification_gates`]) and by
//!   [`verify_shipped_work`]; NEEDLE closes only when both pass. A verdict
//!   that rejected the work releases with failure accounting; a check that
//!   could not run releases *without* penalty (ADR-023: a gate that cannot
//!   run is not a gate that failed).
//! - **Split proposals go through Mitosis.** Child proposals are validated,
//!   deduplicated against the parent's existing children and lineage, capped,
//!   and created as a sequential `split-child` chain under an
//!   umbrella-labelled parent — the manual Auto-Split contract, never
//!   created raw.
//!
//! # Contract
//!
//! `apply` returns `Ok` only when the bead reached a terminal state for this
//! decision (closed, blocked, split, released, or untouched because ownership
//! was lost). `Err` means the decision was *not* applied — invalid proposal,
//! refused split, or a mutation whose safety release also failed (or could
//! not be attempted because ownership could not be confirmed). The caller
//! must not release again after `Ok`: the executor already did it.

use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;

use crate::bead_store::BeadStore;
use crate::config::Config;
use crate::mitosis::MitosisEvaluator;
use crate::outcome::OutcomeHandler;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::{Bead, BeadStatus};
use crate::validation::predispatch;
use crate::validation::verify_shipped_work;

use super::ResolveDecision;

/// How long any single store operation may take before the executor stops
/// waiting and treats it as failed. Mirrors the outcome handler's op timeout:
/// a wedged backend must not hang the HANDLING state.
const OP_TIMEOUT: Duration = Duration::from_secs(30);

/// Upper bound on the guidance note recorded for retry/blocked decisions.
/// The note is a concise operator-facing marker on the bead, not a log dump;
/// the full evidence stays in telemetry and the worker log.
const MAX_NOTE_CHARS: usize = 400;

/// What applying a decision did to the bead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppliedDecision {
    /// Gates and shipped-work verification passed; the bead was closed.
    Completed,
    /// The bead was released back to the ready frontier.
    Released(ReleaseCause),
    /// The concrete prerequisite was recorded and the bead was blocked.
    Blocked,
    /// The split proposal survived Mitosis validation and dedup; children
    /// were created and the parent was blocked pending their completion.
    Split {
        /// Children created (after dedup and the max-children cap).
        created: usize,
        /// Proposals skipped because an existing bead already covers them.
        deduped: usize,
    },
    /// Nothing was mutated: the bead is no longer `in_progress` and assigned
    /// to this worker, so someone else owns its lifecycle now.
    OwnershipLost,
}

impl AppliedDecision {
    /// Machine-readable name for telemetry's `action` field.
    pub fn as_str(&self) -> &'static str {
        match self {
            AppliedDecision::Completed => "completed",
            AppliedDecision::Released(_) => "released",
            AppliedDecision::Blocked => "blocked",
            AppliedDecision::Split { .. } => "split",
            AppliedDecision::OwnershipLost => "ownership_lost",
        }
    }
}

/// Why an applied decision ended in a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseCause {
    /// The resolver said retry: guidance recorded, failure accounting
    /// incremented, normal retry/backoff policy applies.
    Retry,
    /// A configured gate or the shipped-work check judged the work and
    /// rejected it. Released with a failure-count increment.
    Rejected,
    /// A gate or the shipped-work check could not run and judged nothing.
    /// Released without touching the failure count.
    Unverifiable,
    /// Every proposed child already exists — the split is already covered.
    /// Released so the fleet can continue working the parent.
    SplitCovered,
    /// A lifecycle mutation failed mid-application and the safety release
    /// fired. The decision was not applied.
    MutationFailed,
}

impl ReleaseCause {
    /// Machine-readable name for telemetry's `reason` field.
    pub fn as_str(&self) -> &'static str {
        match self {
            ReleaseCause::Retry => "retry",
            ReleaseCause::Rejected => "rejected",
            ReleaseCause::Unverifiable => "unverifiable",
            ReleaseCause::SplitCovered => "split_covered",
            ReleaseCause::MutationFailed => "mutation_failed",
        }
    }
}

/// Applies validated Resolve decisions to the bead lifecycle.
///
/// Owns the mutation side of resolution. Gate running and failure accounting
/// are delegated to the outcome handler so the two paths share one
/// implementation of per-workspace gate resolution and one failure-count
/// contract; split application is delegated to the Mitosis evaluator.
pub struct DecisionExecutor {
    telemetry: Telemetry,
    outcome: OutcomeHandler,
    mitosis: Option<MitosisEvaluator>,
    /// Whether `complete` must pass shipped-work verification before the
    /// executor closes. Same switch as the dispatch path's gate.
    enforce_shipped_work: bool,
    /// Failure count at which a bead quarantines; carried into
    /// `FalseCloseDetected` telemetry for operator context.
    quarantine_threshold: u32,
}

impl DecisionExecutor {
    /// Create an executor from the worker's resolved configuration.
    pub fn new(config: Config, telemetry: Telemetry) -> Self {
        let enforce_shipped_work = config.worker.enforce_shipped_work;
        let quarantine_threshold = config.outcome.quarantine_after_failures;
        DecisionExecutor {
            outcome: OutcomeHandler::new(config, telemetry.clone()),
            telemetry,
            mitosis: None,
            enforce_shipped_work,
            quarantine_threshold,
        }
    }

    /// Provide the Mitosis evaluator used to apply `split` proposals.
    ///
    /// Without one, `split` decisions are refused (and the bead released):
    /// proposals are never created raw, because dedup and the atomic
    /// parent/child dependency policy are what keep a split from duplicating
    /// work or orphaning children.
    pub fn with_mitosis(mut self, mitosis: MitosisEvaluator) -> Self {
        self.mitosis = Some(mitosis);
        self
    }

    /// Apply a validated decision to the bead owned by `actor`.
    ///
    /// `Ok` means the bead reached a terminal state for this decision:
    /// closed, blocked, split, released (including after a judged rejection
    /// or an infrastructure failure whose safety release fired), or untouched
    /// because ownership was lost to another worker. `Err` means the decision
    /// was *refused* — invalid proposal, wrong parent, no Mitosis evaluator —
    /// or a mutation failed and even the safety release could not run; in
    /// both cases the executor has already attempted the best-effort release,
    /// so the caller must not release again.
    ///
    /// `fallback` is the dispatch's in-memory pre-dispatch HEAD baseline,
    /// substituted when the on-disk snapshot is gone — the same handoff the
    /// dispatch path uses for its own shipped-work check.
    pub async fn apply(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        decision: &ResolveDecision,
        actor: &str,
        fallback: Option<&predispatch::PreDispatch>,
    ) -> Result<AppliedDecision> {
        // Defensive re-check of the resolver's own validation. The contract
        // says only validated decisions reach this point; refusing here keeps
        // a malformed decision from driving any mutation.
        if let Err(error) = decision.validate() {
            return Err(self
                .fail_safely(store, bead, actor, "decision failed validation", error)
                .await);
        }

        let applied = match decision {
            ResolveDecision::Complete {
                evidence,
                commit_message,
            } => {
                self.apply_complete(store, bead, actor, evidence, commit_message, fallback)
                    .await
            }
            ResolveDecision::Retry { evidence, strategy } => {
                self.apply_retry(store, bead, actor, evidence, strategy)
                    .await
            }
            ResolveDecision::Blocked {
                evidence,
                blocker_type,
                description,
            } => {
                self.apply_blocked(store, bead, actor, evidence, blocker_type, description)
                    .await
            }
            ResolveDecision::Split {
                evidence,
                parent_bead_id,
                child_titles,
            } => {
                self.apply_split(store, bead, actor, evidence, parent_bead_id, child_titles)
                    .await
            }
        };

        match applied {
            Ok(applied) => {
                // One terminal row per applied decision: what the resolver
                // asked, what the lifecycle did.
                let _ = self.telemetry.emit(
                    EventKind::OutcomeHandled {
                        bead_id: bead.id.clone(),
                        outcome: format!("resolve:{}", decision.as_str()),
                        action: applied.as_str().to_string(),
                    },
                    Utc::now(),
                );
                Ok(applied)
            }
            Err(error) => Err(error),
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // complete
    // ──────────────────────────────────────────────────────────────────────

    /// Close only after the configured gates and shipped-work verification
    /// pass; otherwise release as retryable.
    async fn apply_complete(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        actor: &str,
        evidence: &str,
        commit_message: &str,
        fallback: Option<&predispatch::PreDispatch>,
    ) -> Result<AppliedDecision> {
        if !self.ensure_owned(store, &bead.id, actor).await? {
            return Ok(AppliedDecision::OwnershipLost);
        }

        // Re-read the bead: shipped-work verification judges the store's
        // current state (the agent may have closed it or recorded a note
        // after the resolve decision was formed).
        let current = match self.op(store.show(&bead.id), "show").await {
            Ok(current) => current,
            Err(error) => {
                return self
                    .mutation_failure(store, bead, actor, "show before complete", error)
                    .await
            }
        };

        // Configured gates, resolved from the bead's own workspace — the same
        // machinery the dispatch path uses, so resolve closures are judged by
        // exactly the rules that workspace declares.
        let (verified, gate_report) = match self.outcome.run_verification_gates(bead).await {
            Ok(result) => result,
            Err(error) => {
                // Gates could not even be resolved: nothing judged the work.
                // Release without penalty; the error is reported.
                return self
                    .mutation_failure(store, bead, actor, "gate resolution", error)
                    .await;
            }
        };
        if let Some(report) = gate_report.as_ref().filter(|report| !report.all_passed) {
            if report.results.iter().any(|(_, r)| r.is_execution_error()) {
                // A gate that could not run is not a gate that failed
                // (ADR-023): release without incrementing the failure count.
                tracing::warn!(
                    bead_id = %bead.id,
                    "resolve complete: a gate could not run — releasing without penalty"
                );
                return self
                    .release_owned(store, bead, actor, ReleaseCause::Unverifiable)
                    .await
                    .map(|_| AppliedDecision::Released(ReleaseCause::Unverifiable));
            }
            let failed: Vec<&str> = report
                .results
                .iter()
                .filter(|(_, r)| !r.passed())
                .map(|(name, _)| name.as_str())
                .collect();
            tracing::info!(
                bead_id = %bead.id,
                gates = ?failed,
                "resolve complete: gates rejected the closure — releasing as retryable"
            );
            return self
                .reject_release(store, bead, actor, &format!("gate:{}", failed.join(",")))
                .await;
        }
        if verified {
            let gates_run = gate_report.map(|r| r.results.len()).unwrap_or(0) as u32;
            let _ = self.telemetry.emit(
                EventKind::VerificationPassed {
                    bead_id: bead.id.clone(),
                    gates_run,
                },
                Utc::now(),
            );
        }

        // Shipped-work verification: did this dispatch actually land durable
        // output (or record why none was needed)?
        let mut shipped_verified = false;
        if self.enforce_shipped_work {
            match verify_shipped_work(&current, &bead.workspace, store, fallback).await {
                Ok(crate::validation::GateResult::Pass) => {
                    shipped_verified = true;
                }
                Ok(crate::validation::GateResult::Fail(reason)) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        reason = %reason,
                        "resolve complete: shipped-work check rejected the closure"
                    );
                    return self.reject_release(store, bead, actor, &reason).await;
                }
                Ok(crate::validation::GateResult::Unsatisfiable(reason)) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        reason = %reason,
                        "resolve complete: shipped-work precondition is unsatisfiable — releasing without penalty"
                    );
                    return self
                        .release_owned(store, bead, actor, ReleaseCause::Unverifiable)
                        .await
                        .map(|_| AppliedDecision::Released(ReleaseCause::Unverifiable));
                }
                Ok(crate::validation::GateResult::ExecutionError { command, reason }) => {
                    tracing::warn!(
                        bead_id = %bead.id,
                        command = %command,
                        reason = %reason,
                        "resolve complete: shipped-work check could not run — releasing \
                         without penalty"
                    );
                    return self
                        .release_owned(store, bead, actor, ReleaseCause::Unverifiable)
                        .await
                        .map(|_| AppliedDecision::Released(ReleaseCause::Unverifiable));
                }
                Err(error) => {
                    // The check could not run: nothing judged the closure.
                    // Release without penalty rather than close on no verdict.
                    tracing::warn!(
                        bead_id = %bead.id,
                        error = %error,
                        "resolve complete: shipped-work check errored — releasing without \
                         penalty"
                    );
                    return self
                        .release_owned(store, bead, actor, ReleaseCause::Unverifiable)
                        .await
                        .map(|_| AppliedDecision::Released(ReleaseCause::Unverifiable));
                }
            }
        } else {
            tracing::debug!(
                bead_id = %bead.id,
                "shipped-work enforcement disabled — closing on gates alone"
            );
        }

        // Ownership once more, immediately before the close: everything above
        // is read-only, this is the mutation a stale dispatch must not make.
        if !self.ensure_owned(store, &bead.id, actor).await? {
            return Ok(AppliedDecision::OwnershipLost);
        }
        let reason = format!(
            "closed by NEEDLE resolve: {}",
            concise(commit_message, MAX_NOTE_CHARS)
        );
        match self.op(store.close(&bead.id, &reason), "close").await {
            Ok(_) => {
                tracing::info!(
                    bead_id = %bead.id,
                    evidence = %concise(evidence, 120),
                    "resolve complete: bead closed after gates and shipped-work verification"
                );
                // The failure counter resets only on a verified pass — the
                // same contract as the dispatch path, so a bead that cycles
                // through unverifiable closures still reaches quarantine.
                if shipped_verified {
                    let _ = self.outcome.reset_failure_count(store, bead).await;
                }
                let _ = self.telemetry.emit(
                    EventKind::BeadCompleted {
                        bead_id: bead.id.clone(),
                        duration_ms: 0,
                    },
                    Utc::now(),
                );
                if let Err(error) = self.op(store.flush(), "flush").await {
                    store.pause_workspace(format!(
                        "checkpoint publication failed after resolve close: {error:#}"
                    ));
                    return Err(error.context(
                        "checkpoint publication failed after resolve close; completion is \
                         unverified",
                    ));
                }
                Ok(AppliedDecision::Completed)
            }
            Err(error) => {
                self.mutation_failure(store, bead, actor, "close", error)
                    .await
            }
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // retry
    // ──────────────────────────────────────────────────────────────────────

    /// Record concise retry guidance, increment failure accounting, and
    /// release with the normal retry/backoff policy.
    async fn apply_retry(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        actor: &str,
        evidence: &str,
        strategy: &str,
    ) -> Result<AppliedDecision> {
        if !self.ensure_owned(store, &bead.id, actor).await? {
            return Ok(AppliedDecision::OwnershipLost);
        }

        // Guidance is best-effort: a backend without notes support must still
        // release, or the bead wedges in `in_progress` forever.
        let note = format!(
            "resolve retry ({}): {}",
            concise(strategy, 120),
            concise(evidence, MAX_NOTE_CHARS)
        );
        if let Err(error) = self
            .op(store.append_notes(&bead.id, &note), "append_notes")
            .await
        {
            tracing::warn!(
                bead_id = %bead.id,
                error = %error,
                "resolve retry: could not record guidance note — releasing anyway"
            );
        }

        if !self.ensure_owned(store, &bead.id, actor).await? {
            return Ok(AppliedDecision::OwnershipLost);
        }
        // Failure accounting rides the outcome handler's implementation: the
        // failure-count label, the degraded-window marker, and the fleet-wide
        // retry cooldown all stay in one place.
        let count = self
            .outcome
            .increment_failure_count(store, bead)
            .await
            .unwrap_or(0);
        tracing::info!(
            bead_id = %bead.id,
            failure_count = count,
            strategy = %concise(strategy, 120),
            "resolve retry: guidance recorded, failure accounting incremented — releasing"
        );

        match self
            .release_owned(store, bead, actor, ReleaseCause::Retry)
            .await
        {
            Ok(true) => Ok(AppliedDecision::Released(ReleaseCause::Retry)),
            Ok(false) => Ok(AppliedDecision::OwnershipLost),
            Err(error) => Err(error),
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // blocked
    // ──────────────────────────────────────────────────────────────────────

    /// Record the concrete external prerequisite and move the bead to the
    /// backend's blocked state.
    async fn apply_blocked(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        actor: &str,
        evidence: &str,
        blocker_type: &str,
        description: &str,
    ) -> Result<AppliedDecision> {
        if !self.ensure_owned(store, &bead.id, actor).await? {
            return Ok(AppliedDecision::OwnershipLost);
        }

        // Same best-effort contract as retry: the block is the transition
        // that stops the loop; the note is the operator-facing record.
        let note = format!(
            "resolve blocked on {}: {} (evidence: {})",
            concise(blocker_type, 80),
            concise(description, MAX_NOTE_CHARS),
            concise(evidence, MAX_NOTE_CHARS)
        );
        if let Err(error) = self
            .op(store.append_notes(&bead.id, &note), "append_notes")
            .await
        {
            tracing::warn!(
                bead_id = %bead.id,
                error = %error,
                "resolve blocked: could not record prerequisite note — blocking anyway"
            );
        }

        if !self.ensure_owned(store, &bead.id, actor).await? {
            return Ok(AppliedDecision::OwnershipLost);
        }
        match self.op(store.block(&bead.id), "block").await {
            Ok(_) => {
                tracing::info!(
                    bead_id = %bead.id,
                    blocker_type = %concise(blocker_type, 80),
                    "resolve blocked: bead moved to the backend's blocked state"
                );
                Ok(AppliedDecision::Blocked)
            }
            Err(error) => {
                self.mutation_failure(store, bead, actor, "block", error)
                    .await
            }
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // split
    // ──────────────────────────────────────────────────────────────────────

    /// Send the child proposal through Mitosis validation and deduplication,
    /// then apply the parent/child dependency policy. Ownership is re-checked
    /// immediately before child creation begins, so a dispatch that lost its
    /// claim while validating creates nothing.
    async fn apply_split(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        actor: &str,
        evidence: &str,
        parent_bead_id: &str,
        child_titles: &[String],
    ) -> Result<AppliedDecision> {
        if !self.ensure_owned(store, &bead.id, actor).await? {
            return Ok(AppliedDecision::OwnershipLost);
        }

        // The executor only ever mutates the bead it holds the claim on. A
        // decision naming a different parent is malformed for this dispatch —
        // refusing is the safety property, whoever owns `parent_bead_id` gets
        // to decide its lifecycle.
        if parent_bead_id != bead.id.as_ref() {
            let error = anyhow::anyhow!(
                "split decision names parent {} but the dispatch holds {}",
                parent_bead_id,
                bead.id
            );
            return Err(self
                .fail_safely(store, bead, actor, "split parent mismatch", error)
                .await);
        }

        let Some(mitosis) = self.mitosis.as_ref() else {
            let error = anyhow::anyhow!(
                "no Mitosis evaluator configured — split proposals are never created raw"
            );
            return Err(self
                .fail_safely(store, bead, actor, "split unavailable", error)
                .await);
        };

        // Ownership once more, immediately before children start being
        // created: everything since the entry check was read-only, and child
        // creation is the mutation a stale dispatch must not begin. (The
        // check cannot cover the creation loop itself — the backend has no
        // fence — but it narrows the window to the loop's own duration.)
        if !self.ensure_owned(store, &bead.id, actor).await? {
            return Ok(AppliedDecision::OwnershipLost);
        }

        let application = mitosis
            .apply_split_proposals(store, bead, child_titles, evidence)
            .await;
        match application {
            Ok(crate::mitosis::SplitApplication::Applied { created, deduped }) => {
                tracing::info!(
                    bead_id = %bead.id,
                    created,
                    deduped,
                    "resolve split: children created — blocking the parent pending them"
                );
                // A split parent waits on its children rather than returning
                // to the ready frontier, where it would be re-dispatched and
                // re-split (the plan's "split and block parent" policy).
                if !self.ensure_owned(store, &bead.id, actor).await? {
                    return Ok(AppliedDecision::OwnershipLost);
                }
                match self.op(store.block(&bead.id), "block").await {
                    Ok(_) => Ok(AppliedDecision::Split { created, deduped }),
                    Err(error) => {
                        self.mutation_failure(store, bead, actor, "block after split", error)
                            .await
                    }
                }
            }
            Ok(crate::mitosis::SplitApplication::FullyDeduplicated { covered }) => {
                tracing::info!(
                    bead_id = %bead.id,
                    covered,
                    "resolve split: every proposed child already exists — releasing the parent"
                );
                match self
                    .release_owned(store, bead, actor, ReleaseCause::SplitCovered)
                    .await
                {
                    Ok(true) => Ok(AppliedDecision::Released(ReleaseCause::SplitCovered)),
                    Ok(false) => Ok(AppliedDecision::OwnershipLost),
                    Err(error) => Err(error),
                }
            }
            Ok(crate::mitosis::SplitApplication::Refused { reason }) => {
                let error = anyhow::anyhow!("split proposal refused: {reason}");
                Err(self
                    .fail_safely(store, bead, actor, "split refused", error)
                    .await)
            }
            Err(error) => {
                self.mutation_failure(store, bead, actor, "split application", error)
                    .await
            }
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // ownership + safety plumbing
    // ──────────────────────────────────────────────────────────────────────

    /// True when the live store still shows the bead `in_progress` and
    /// assigned to `actor`. Called before every mutation: a dispatch that
    /// lost its claim must not mutate a bead that changed hands.
    async fn ensure_owned(
        &self,
        store: &dyn BeadStore,
        bead_id: &crate::types::BeadId,
        actor: &str,
    ) -> Result<bool> {
        let status = self.op(store.claim_status(bead_id), "claim_status").await?;
        let owned =
            status.status == BeadStatus::InProgress && status.assignee.as_deref() == Some(actor);
        if !owned {
            tracing::warn!(
                bead_id = %bead_id,
                expected_actor = %actor,
                actual_status = ?status.status,
                actual_assignee = ?status.assignee,
                "resolve: ownership re-check failed — no mutation will be attempted"
            );
            let _ = self.telemetry.emit(
                EventKind::ClaimRecheckFailed {
                    bead_id: bead_id.clone(),
                    expected_actor: actor.to_string(),
                    stage: "resolve".to_string(),
                    actual_status: format!("{:?}", status.status),
                    actual_assignee: status
                        .assignee
                        .clone()
                        .unwrap_or_else(|| "(none)".to_string()),
                },
                Utc::now(),
            );
        }
        Ok(owned)
    }

    /// Release the bead if — and only if — it is still ours.
    ///
    /// Returns `Ok(true)` when the release was performed, `Ok(false)` when
    /// ownership was lost (the new owner owns the transition now), and `Err`
    /// when the bead is still ours but could not be released — the one state
    /// the caller must hear about.
    async fn release_owned(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        actor: &str,
        cause: ReleaseCause,
    ) -> Result<bool> {
        if !self.ensure_owned(store, &bead.id, actor).await? {
            return Ok(false);
        }
        match self.op(store.release(&bead.id), "release").await {
            Ok(_) => {
                tracing::info!(
                    bead_id = %bead.id,
                    cause = %cause.as_str(),
                    "resolve: bead released"
                );
                let _ = self.telemetry.emit(
                    EventKind::BeadReleased {
                        bead_id: bead.id.clone(),
                        reason: cause.as_str().to_string(),
                    },
                    Utc::now(),
                );
                Ok(true)
            }
            Err(error) => {
                let _ = self.telemetry.emit(
                    EventKind::BeadReleaseFailed {
                        bead_id: bead.id.clone(),
                        reason: cause.as_str().to_string(),
                    },
                    Utc::now(),
                );
                Err(error.context(format!(
                    "resolve: failed to release bead {} after applying a decision ({}) — \
                     the dispatch may leave it in_progress",
                    bead.id,
                    cause.as_str()
                )))
            }
        }
    }

    /// Release with a failure-count increment: the work was judged and
    /// rejected, so the normal retry/quarantine ladder applies.
    async fn reject_release(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        actor: &str,
        reason: &str,
    ) -> Result<AppliedDecision> {
        // Failure accounting is itself a lifecycle mutation. Re-check before
        // touching labels so a dispatch that lost its claim while the gate or
        // shipped-work check ran cannot penalize a new owner.
        if !self.ensure_owned(store, &bead.id, actor).await? {
            return Ok(AppliedDecision::OwnershipLost);
        }
        let count = self
            .outcome
            .increment_failure_count(store, bead)
            .await
            .unwrap_or(0);
        let _ = self.telemetry.emit(
            EventKind::FalseCloseDetected {
                bead_id: bead.id.clone(),
                failure_count: count,
                threshold: self.quarantine_threshold,
                reason: reason.to_string(),
            },
            Utc::now(),
        );
        match self
            .release_owned(store, bead, actor, ReleaseCause::Rejected)
            .await
        {
            Ok(true) => Ok(AppliedDecision::Released(ReleaseCause::Rejected)),
            Ok(false) => Ok(AppliedDecision::OwnershipLost),
            Err(error) => Err(error),
        }
    }

    /// The safety net behind an infrastructure mutation failure: attempt an
    /// ownership-checked release so the dispatch never silently leaves its
    /// bead `in_progress`, and report the terminal outcome. The decision was
    /// not applied, but the bead reached a defined state — that is `Ok` for
    /// the caller, with [`AppliedDecision::Released`] carrying the cause.
    ///
    /// The release is deliberately *without* failure accounting: nothing here
    /// judged the work, so no penalty applies (the callers that have a
    /// judgment increment before releasing via [`Self::reject_release`]).
    async fn mutation_failure(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        actor: &str,
        stage: &str,
        error: anyhow::Error,
    ) -> Result<AppliedDecision> {
        tracing::warn!(
            bead_id = %bead.id,
            stage = %stage,
            error = %error,
            "resolve: lifecycle mutation failed — attempting the safety release"
        );
        match self
            .release_owned(store, bead, actor, ReleaseCause::MutationFailed)
            .await
        {
            Ok(true) => Ok(AppliedDecision::Released(ReleaseCause::MutationFailed)),
            Ok(false) => Ok(AppliedDecision::OwnershipLost),
            Err(release_error) => Err(error.context(format!(
                "resolve decision not applied (failed at {stage}) AND the safety release \
                 failed: {release_error:#} — the bead may still be in_progress"
            ))),
        }
    }

    /// The safety net behind a *refusal*: the decision itself is invalid or
    /// violates policy (bad proposal, wrong parent, no Mitosis evaluator), so
    /// the caller must hear about it — but the dispatch still releases its
    /// claim first, per the documented `resolution_failed` fallback.
    ///
    /// Unlike [`Self::mutation_failure`] this always returns `Err`: a refused
    /// decision is a contract violation to surface, not a lifecycle outcome
    /// to swallow.
    async fn fail_safely(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        actor: &str,
        stage: &str,
        error: anyhow::Error,
    ) -> anyhow::Error {
        tracing::warn!(
            bead_id = %bead.id,
            stage = %stage,
            error = %error,
            "resolve: decision refused — releasing the claim and surfacing the refusal"
        );
        match self
            .release_owned(store, bead, actor, ReleaseCause::MutationFailed)
            .await
        {
            Ok(true) => error.context(format!(
                "resolve decision refused (failed at {stage}); bead released as \
                 resolution_failed"
            )),
            Ok(false) => error.context(format!(
                "resolve decision refused (failed at {stage}); bead changed hands \
                 before the safety release, leaving it to its current owner"
            )),
            Err(release_error) => error.context(format!(
                "resolve decision refused (failed at {stage}) AND the safety release \
                 failed: {release_error:#} — the bead may still be in_progress"
            )),
        }
    }

    /// Run one store operation under the HANDLING timeout.
    async fn op<T>(&self, fut: impl Future<Output = Result<T>>, name: &str) -> Result<T> {
        match tokio::time::timeout(OP_TIMEOUT, fut).await {
            Ok(result) => result.with_context(|| format!("store operation {name} failed")),
            Err(_) => Err(anyhow::anyhow!(
                "store operation {name} timed out after {}s",
                OP_TIMEOUT.as_secs()
            )),
        }
    }
}

/// Collapse a string to a single concise line, capped at `max` characters.
///
/// Notes recorded on beads are operator-facing; multi-line agent evidence is
/// collapsed so the note stays one greppable line.
fn concise(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let truncated: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{truncated}…")
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::Filters;
    use crate::types::{BeadId, ClaimResult};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, Mutex as StateMutex};

    /// Records every lifecycle mutation the executor performs, and can fail
    /// or change ownership on cue to exercise the race and failure paths.
    struct RecordingStore {
        claim: StateMutex<ClaimState>,
        /// After this many `claim_status` reads, reassign the bead to another
        /// worker — a deterministic ownership race.
        flip_assignee_after: Option<usize>,
        /// On the `claim_status` read with this 0-based index, another
        /// worker's *complete* decision wins the race and closes the bead
        /// mid-flight.
        closed_by_winner_after_read: Option<usize>,
        claim_reads: AtomicUsize,
        notes: Mutex<Vec<String>>,
        releases: AtomicUsize,
        blocks: AtomicUsize,
        closes: Mutex<Vec<String>>,
        labels: Mutex<Vec<String>>,
        created_children: Mutex<Vec<String>>,
        /// The split path's children, keyed by id: labels and (once
        /// compensated) the close reason.
        child_beads: StateMutex<HashMap<String, ChildBead>>,
        dependencies: Mutex<Vec<(String, String)>>,
        fail_append_notes: bool,
        fail_release: bool,
        fail_block: bool,
        fail_close: bool,
        /// `create_bead` fails once this many children already exist.
        fail_create_after: Option<usize>,
        /// A *parent* `add_label` fails once this many parent labels have
        /// already landed — the second umbrella label failing after the
        /// first applied.
        fail_parent_label_after: Option<usize>,
        /// Armed once: the next store op of this kind fails — `"dep"` fails
        /// the next `add_dependency`, `"label"` the next parent `add_label`.
        armed_failure: StateMutex<Option<&'static str>>,
        /// What `notes()` reports — the shipped-work gate reads it.
        stored_notes: String,
        /// Beads beyond the parent that `list_all` reports — existing mitosis
        /// children for the dedup tests.
        extra_beads: Vec<Bead>,
        workspace: PathBuf,
    }

    /// A child bead the split path created through this store.
    #[derive(Clone, Debug, Default)]
    struct ChildBead {
        labels: Vec<String>,
        /// Set by the compensating close of an aborted split.
        closed: Option<String>,
    }

    /// The bead id every `RecordingStore` models (`RecordingStore::bead`).
    const PARENT_ID: &str = "needle-exec";

    #[derive(Clone, Debug)]
    struct ClaimState {
        status: BeadStatus,
        assignee: Option<String>,
    }

    impl RecordingStore {
        fn new(workspace: PathBuf) -> Self {
            RecordingStore {
                claim: StateMutex::new(ClaimState {
                    status: BeadStatus::InProgress,
                    assignee: Some("worker-a".to_string()),
                }),
                flip_assignee_after: None,
                closed_by_winner_after_read: None,
                claim_reads: AtomicUsize::new(0),
                notes: Mutex::new(Vec::new()),
                releases: AtomicUsize::new(0),
                blocks: AtomicUsize::new(0),
                closes: Mutex::new(Vec::new()),
                labels: Mutex::new(Vec::new()),
                created_children: Mutex::new(Vec::new()),
                child_beads: StateMutex::new(HashMap::new()),
                dependencies: Mutex::new(Vec::new()),
                fail_append_notes: false,
                fail_release: false,
                fail_block: false,
                fail_close: false,
                fail_create_after: None,
                fail_parent_label_after: None,
                armed_failure: StateMutex::new(None),
                stored_notes: String::new(),
                extra_beads: Vec::new(),
                workspace,
            }
        }

        /// `create_bead` fails once `count` children already exist — a
        /// deterministic mid-creation failure.
        fn failing_create_after(mut self, count: usize) -> Self {
            self.fail_create_after = Some(count);
            self
        }

        /// A parent `add_label` fails once `count` parent labels have already
        /// landed — the second umbrella label failing after the first applied.
        fn failing_parent_label_after(mut self, count: usize) -> Self {
            self.fail_parent_label_after = Some(count);
            self
        }

        /// Arm a one-shot failure: the next `add_dependency` (`"dep"`) or
        /// parent `add_label` (`"label"`) fails.
        fn arm_failure(self, kind: &'static str) -> Self {
            *self.armed_failure.lock().unwrap() = Some(kind);
            self
        }

        /// A concurrent complete decision wins the race: on the
        /// `claim_status` read with 0-based index `n`, the bead is closed by
        /// its (new) owner.
        fn closed_by_winner_after_n_reads(mut self, n: usize) -> Self {
            self.closed_by_winner_after_read = Some(n);
            self
        }

        /// Seed an existing bead that `list_all` reports — e.g. a mitosis
        /// child from a previous split.
        fn with_existing_child(mut self, mut bead: Bead) -> Self {
            bead.workspace = self.workspace.clone();
            self.extra_beads.push(bead);
            self
        }

        fn with_stored_notes(mut self, notes: &str) -> Self {
            self.stored_notes = notes.to_string();
            self
        }

        fn with_labels(mut self, labels: &[&str]) -> Self {
            self.labels = Mutex::new(labels.iter().map(|label| (*label).to_string()).collect());
            self
        }

        /// Hand the bead to `worker-b` on the `claim_status` read whose
        /// 0-based index is `n` — a deterministic ownership race. Reads: 0 is
        /// the executor's entry check; later reads are its re-checks before
        /// each mutation.
        fn flips_to_foreign_owner_after_n_reads(mut self, n: usize) -> Self {
            self.flip_assignee_after = Some(n);
            self
        }

        fn released(&self) -> usize {
            self.releases.load(Ordering::SeqCst)
        }

        fn labels_snapshot(&self) -> Vec<String> {
            self.labels.lock().unwrap().clone()
        }

        fn closes_snapshot(&self) -> Vec<String> {
            self.closes.lock().unwrap().clone()
        }

        fn notes_snapshot(&self) -> Vec<String> {
            self.notes.lock().unwrap().clone()
        }

        fn children_snapshot(&self) -> Vec<String> {
            self.created_children.lock().unwrap().clone()
        }

        fn deps_snapshot(&self) -> Vec<(String, String)> {
            self.dependencies.lock().unwrap().clone()
        }

        /// Labels the split path gave the child with this id.
        fn child_labels(&self, id: &str) -> Vec<String> {
            self.child_beads
                .lock()
                .unwrap()
                .get(id)
                .map(|child| child.labels.clone())
                .unwrap_or_default()
        }

        /// Close reasons recorded for child beads — the compensating closes
        /// of an aborted split.
        fn child_close_reasons(&self) -> Vec<String> {
            self.child_beads
                .lock()
                .unwrap()
                .values()
                .filter_map(|child| child.closed.clone())
                .collect()
        }

        /// Child beads still open — a compensated split leaves none.
        fn open_child_ids(&self) -> Vec<String> {
            self.child_beads
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, child)| child.closed.is_none())
                .map(|(id, _)| id.clone())
                .collect()
        }

        /// Consume the armed one-shot failure: `Ok(())` to proceed, `Err` to
        /// fail the calling op.
        fn take_armed_failure(&self, kind: &str) -> Result<()> {
            let mut armed = self.armed_failure.lock().unwrap();
            let matches = armed.as_deref() == Some(kind);
            if matches {
                *armed = None;
                anyhow::bail!("injected {kind} failure");
            }
            Ok(())
        }
    }

    impl RecordingStore {
        fn bead(&self) -> Bead {
            Bead {
                id: BeadId::from("needle-exec"),
                title: "Executor test bead".to_string(),
                body: Some("Body".to_string()),
                priority: 1,
                status: BeadStatus::InProgress,
                assignee: Some("worker-a".to_string()),
                labels: self.labels_snapshot(),
                workspace: self.workspace.clone(),
                dependencies: vec![],
                dependents: vec![],
                comments: vec![],
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for RecordingStore {
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn list_all(&self) -> Result<Vec<Bead>> {
            let mut all = vec![self.bead()];
            all.extend(self.extra_beads.clone());
            Ok(all)
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            let mut bead = self.bead();
            let claim = self.claim.lock().unwrap();
            bead.status = claim.status.clone();
            bead.assignee = claim.assignee.clone();
            Ok(bead)
        }
        async fn notes(&self, _id: &BeadId) -> Result<Option<String>> {
            Ok(Some(self.stored_notes.clone()))
        }
        async fn claim_status(&self, _id: &BeadId) -> Result<crate::types::ClaimStatus> {
            let n = self.claim_reads.fetch_add(1, Ordering::SeqCst);
            if Some(n) == self.flip_assignee_after {
                let mut claim = self.claim.lock().unwrap();
                claim.assignee = Some("worker-b".to_string());
            }
            if Some(n) == self.closed_by_winner_after_read {
                let mut claim = self.claim.lock().unwrap();
                claim.status = BeadStatus::Done;
                claim.assignee = None;
            }
            let claim = self.claim.lock().unwrap();
            Ok(crate::types::ClaimStatus {
                status: claim.status.clone(),
                assignee: claim.assignee.clone(),
                revision: None,
                claim_epoch: None,
            })
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not used")
        }
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "mock".to_string(),
            })
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
            if self.fail_release {
                anyhow::bail!("release failed (injected)");
            }
            self.releases.fetch_add(1, Ordering::SeqCst);
            let mut claim = self.claim.lock().unwrap();
            claim.status = BeadStatus::Open;
            claim.assignee = None;
            Ok(())
        }
        async fn block(&self, _id: &BeadId) -> Result<()> {
            if self.fail_block {
                anyhow::bail!("block failed (injected)");
            }
            self.blocks.fetch_add(1, Ordering::SeqCst);
            let mut claim = self.claim.lock().unwrap();
            claim.status = BeadStatus::Deferred;
            Ok(())
        }
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
        async fn reopen(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn close(&self, id: &BeadId, reason: &str) -> Result<()> {
            if id.as_ref() != PARENT_ID {
                // A child bead — the compensating close of an aborted split.
                // Never touches the parent's claim state.
                if let Some(child) = self.child_beads.lock().unwrap().get_mut(id.as_ref()) {
                    child.closed = Some(reason.to_string());
                }
                return Ok(());
            }
            if self.fail_close {
                anyhow::bail!("close failed (injected)");
            }
            self.closes.lock().unwrap().push(reason.to_string());
            let mut claim = self.claim.lock().unwrap();
            claim.status = BeadStatus::Done;
            Ok(())
        }
        async fn append_notes(&self, _id: &BeadId, note: &str) -> Result<()> {
            if self.fail_append_notes {
                anyhow::bail!("backend does not implement append_notes");
            }
            self.notes.lock().unwrap().push(note.to_string());
            Ok(())
        }
        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            Ok(self.labels_snapshot())
        }
        async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
            if id.as_ref() != PARENT_ID {
                if let Some(child) = self.child_beads.lock().unwrap().get_mut(id.as_ref()) {
                    child.labels.push(label.to_string());
                }
                return Ok(());
            }
            self.take_armed_failure("label")?;
            if let Some(limit) = self.fail_parent_label_after {
                if self.labels.lock().unwrap().len() >= limit {
                    anyhow::bail!("parent label failed (injected after {limit} labels)");
                }
            }
            self.labels.lock().unwrap().push(label.to_string());
            Ok(())
        }
        async fn remove_label(&self, _id: &BeadId, label: &str) -> Result<()> {
            self.labels.lock().unwrap().retain(|l| l != label);
            Ok(())
        }
        async fn create_bead(&self, title: &str, _body: &str, labels: &[&str]) -> Result<BeadId> {
            if let Some(limit) = self.fail_create_after {
                if self.child_beads.lock().unwrap().len() >= limit {
                    anyhow::bail!("create failed (injected after {limit} children)");
                }
            }
            let id = format!("child-{}", self.child_beads.lock().unwrap().len() + 1);
            self.child_beads.lock().unwrap().insert(
                id.clone(),
                ChildBead {
                    labels: labels.iter().map(|l| l.to_string()).collect(),
                    closed: None,
                },
            );
            self.created_children
                .lock()
                .unwrap()
                .push(title.to_string());
            Ok(BeadId::from(id))
        }
        async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
            self.take_armed_failure("dep")?;
            self.dependencies
                .lock()
                .unwrap()
                .push((blocker_id.to_string(), blocked_id.to_string()));
            Ok(())
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
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
        fn has_valid_store(&self) -> bool {
            true
        }
    }

    fn executor() -> DecisionExecutor {
        DecisionExecutor::new(Config::default(), Telemetry::new("test".to_string()))
    }

    fn temp_workspace() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    fn complete_decision() -> ResolveDecision {
        ResolveDecision::Complete {
            evidence: "tests passed".to_string(),
            commit_message: "fix: the thing".to_string(),
        }
    }

    fn retry_decision() -> ResolveDecision {
        ResolveDecision::Retry {
            evidence: "rate limited by the provider".to_string(),
            strategy: "back off and retry".to_string(),
        }
    }

    fn blocked_decision() -> ResolveDecision {
        ResolveDecision::Blocked {
            evidence: "the API returns 404".to_string(),
            blocker_type: "external dependency".to_string(),
            description: "upstream service must publish schema v2".to_string(),
        }
    }

    fn split_decision(parent: &str, titles: &[&str]) -> ResolveDecision {
        ResolveDecision::Split {
            evidence: "three independent deliverables".to_string(),
            parent_bead_id: parent.to_string(),
            child_titles: titles.iter().map(|s| s.to_string()).collect(),
        }
    }

    async fn apply(
        executor: &DecisionExecutor,
        store: &RecordingStore,
        decision: &ResolveDecision,
    ) -> Result<AppliedDecision> {
        let bead = store.bead();
        executor
            .apply(store, &bead, decision, "worker-a", None)
            .await
    }

    // ── complete ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn complete_closes_after_gates_and_shipped_work_pass() {
        // No .needle.yaml in the workspace → no configured gates → passes.
        // A non-empty bead note satisfies the shipped-work gate's note arm
        // (no snapshot recorded), so no git repository is needed here.
        let (_dir, workspace) = temp_workspace();
        let store = RecordingStore::new(workspace).with_stored_notes("did the work; see notes");

        let applied = apply(&executor(), &store, &complete_decision())
            .await
            .expect("complete should close");

        assert_eq!(applied, AppliedDecision::Completed);
        let closes = store.closes_snapshot();
        assert_eq!(closes.len(), 1, "exactly one close");
        assert!(
            closes[0].contains("fix: the thing"),
            "close carries the commit message"
        );
        assert_eq!(store.released(), 0, "nothing released");
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0, "nothing blocked");

        complete_verified_success_resets_failure_accounting().await;
    }

    #[tokio::test]
    async fn complete_releases_with_failure_count_when_shipped_work_fails() {
        let (_dir, workspace) = temp_workspace();
        // Empty notes and no snapshot: the shipped-work gate finds neither a
        // commit nor a note and rejects the closure.
        let store = RecordingStore::new(workspace);

        let applied = apply(&executor(), &store, &complete_decision())
            .await
            .expect("shipped-work rejection releases, does not error");

        assert_eq!(applied, AppliedDecision::Released(ReleaseCause::Rejected));
        assert!(
            store.closes_snapshot().is_empty(),
            "no close without shipped work"
        );
        assert_eq!(store.released(), 1);
        assert!(
            store
                .labels_snapshot()
                .iter()
                .any(|l| l == "failure-count:1"),
            "a false close increments the failure count"
        );
    }

    #[tokio::test]
    async fn complete_releases_with_failure_count_when_configured_gate_fails() {
        let (_dir, workspace) = temp_workspace();
        std::fs::write(
            workspace.join(".needle.yaml"),
            "gates:\n  - type: command\n    commands:\n      - 'false'\n    run_in: workspace\n",
        )
        .expect("write gate configuration");
        // The configured gate rejects before shipped-work verification can
        // close the bead, even though the bead carries work evidence.
        let store = RecordingStore::new(workspace).with_stored_notes("did the work");

        let applied = apply(&executor(), &store, &complete_decision())
            .await
            .expect("gate rejection releases, does not error");

        assert_eq!(applied, AppliedDecision::Released(ReleaseCause::Rejected));
        assert!(
            store.closes_snapshot().is_empty(),
            "a failed configured gate must prevent close"
        );
        assert_eq!(store.released(), 1);
        assert!(
            store
                .labels_snapshot()
                .iter()
                .any(|label| label == "failure-count:1"),
            "a judged gate failure increments the failure count"
        );

        complete_race_before_rejection_accounting_does_not_penalize_new_owner().await;
    }

    async fn complete_race_before_rejection_accounting_does_not_penalize_new_owner() {
        let (_dir, workspace) = temp_workspace();
        std::fs::write(
            workspace.join(".needle.yaml"),
            "gates:\n  - type: command\n    commands:\n      - 'false'\n    run_in: workspace\n",
        )
        .expect("write gate configuration");
        // The entry check passes; the re-check in reject_release observes the
        // handoff before failure accounting or release can touch the bead.
        let store = RecordingStore::new(workspace)
            .with_stored_notes("did the work")
            .flips_to_foreign_owner_after_n_reads(1);

        let applied = apply(&executor(), &store, &complete_decision())
            .await
            .expect("a lost race is a normal outcome");

        assert_eq!(applied, AppliedDecision::OwnershipLost);
        assert!(store.closes_snapshot().is_empty());
        assert!(store
            .labels_snapshot()
            .iter()
            .all(|label| !label.starts_with("failure-count:")));
        assert_eq!(store.released(), 0, "never release the new owner's claim");
    }

    async fn complete_verified_success_resets_failure_accounting() {
        let (_dir, workspace) = temp_workspace();
        let store = RecordingStore::new(workspace)
            .with_stored_notes("did the work")
            .with_labels(&[
                "failure-count:3",
                "quarantine-until:2099-01-01T00:00:00Z",
                "quarantine-round:3",
            ]);

        let applied = apply(&executor(), &store, &complete_decision())
            .await
            .expect("verified complete should close");

        assert_eq!(applied, AppliedDecision::Completed);
        assert!(store.labels_snapshot().iter().all(|label| {
            !label.starts_with("failure-count:")
                && !label.starts_with("quarantine-until:")
                && !label.starts_with("quarantine-round:")
        }));
    }

    #[tokio::test]
    async fn complete_race_ownership_lost_before_close_mutates_nothing() {
        let (_dir, workspace) = temp_workspace();
        // The first ownership check (apply entry) passes; the bead is then
        // reassigned to worker-b; the pre-close re-check must catch it.
        let store = RecordingStore::new(workspace)
            .with_stored_notes("did the work")
            .flips_to_foreign_owner_after_n_reads(1);

        let applied = apply(&executor(), &store, &complete_decision())
            .await
            .expect("a lost race is a normal outcome, not an error");

        assert_eq!(applied, AppliedDecision::OwnershipLost);
        assert!(
            store.closes_snapshot().is_empty(),
            "a stale dispatch must not close"
        );
        assert_eq!(
            store.released(),
            0,
            "a stale dispatch must not release either"
        );
    }

    #[tokio::test]
    async fn complete_close_failure_releases_safely_without_penalty() {
        let (_dir, workspace) = temp_workspace();
        let mut store = RecordingStore::new(workspace).with_stored_notes("did the work");
        store.fail_close = true;

        let applied = apply(&executor(), &store, &complete_decision())
            .await
            .expect("the safety release succeeded, so the outcome is terminal");

        assert_eq!(
            applied,
            AppliedDecision::Released(ReleaseCause::MutationFailed),
            "a failed close ends in the safety release"
        );
        assert_eq!(store.released(), 1, "the bead was not left in_progress");
        assert!(
            !store
                .labels_snapshot()
                .iter()
                .any(|l| l.starts_with("failure-count:")),
            "an infrastructure failure carries no failure increment"
        );
    }

    #[tokio::test]
    async fn complete_close_and_release_failure_surfaces_an_error() {
        let (_dir, workspace) = temp_workspace();
        let mut store = RecordingStore::new(workspace).with_stored_notes("did the work");
        store.fail_close = true;
        store.fail_release = true;

        let result = apply(&executor(), &store, &complete_decision()).await;

        let error = result.expect_err("when even the safety release fails, the caller must hear");
        assert!(
            format!("{error:#}").contains("safety release failed"),
            "the error names the double failure, got: {error:#}"
        );
        assert!(store.closes_snapshot().is_empty());
    }

    // ── retry ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn retry_records_guidance_increments_and_releases() {
        let (_dir, workspace) = temp_workspace();
        let store = RecordingStore::new(workspace);

        let applied = apply(&executor(), &store, &retry_decision())
            .await
            .expect("retry applies");

        assert_eq!(applied, AppliedDecision::Released(ReleaseCause::Retry));
        let notes = store.notes_snapshot();
        assert_eq!(notes.len(), 1, "one concise guidance note");
        assert!(
            notes[0].contains("back off and retry"),
            "guidance carries the strategy"
        );
        assert!(
            notes[0].contains("rate limited"),
            "guidance carries the evidence"
        );
        assert!(
            store
                .labels_snapshot()
                .iter()
                .any(|l| l == "failure-count:1"),
            "failure accounting incremented"
        );
        assert!(
            store
                .labels_snapshot()
                .iter()
                .any(|l| l.starts_with("quarantine-until:")),
            "the fleet-wide retry cooldown is applied"
        );
        assert_eq!(store.released(), 1, "bead released for retry");
    }

    #[tokio::test]
    async fn retry_still_releases_when_note_recording_fails() {
        let (_dir, workspace) = temp_workspace();
        let mut store = RecordingStore::new(workspace);
        store.fail_append_notes = true;

        let applied = apply(&executor(), &store, &retry_decision())
            .await
            .expect("a backend without notes must not wedge the bead");

        assert_eq!(applied, AppliedDecision::Released(ReleaseCause::Retry));
        assert!(store.notes_snapshot().is_empty());
        assert!(
            store
                .labels_snapshot()
                .iter()
                .any(|l| l == "failure-count:1"),
            "accounting still incremented"
        );
        assert_eq!(
            store.released(),
            1,
            "the essential transition still happened"
        );

        retry_release_failure_is_reported_after_accounting().await;
    }

    #[tokio::test]
    async fn retry_race_ownership_lost_touches_nothing() {
        let (_dir, workspace) = temp_workspace();
        let store = RecordingStore::new(workspace).flips_to_foreign_owner_after_n_reads(0);

        let applied = apply(&executor(), &store, &retry_decision())
            .await
            .expect("a lost race is a normal outcome");

        assert_eq!(applied, AppliedDecision::OwnershipLost);
        assert!(
            store.notes_snapshot().is_empty(),
            "no note on a foreign bead"
        );
        assert!(
            !store
                .labels_snapshot()
                .iter()
                .any(|l| l.starts_with("failure-count:")),
            "no accounting on a foreign bead"
        );
        assert_eq!(store.released(), 0, "never release someone else's claim");
    }

    #[tokio::test]
    async fn retry_race_before_release_leaves_the_new_owner_alone() {
        let (_dir, workspace) = temp_workspace();
        // Ownership flips after the entry check and the accounting check both
        // passed; the pre-release re-check must stop the release.
        let store = RecordingStore::new(workspace).flips_to_foreign_owner_after_n_reads(2);

        let applied = apply(&executor(), &store, &retry_decision())
            .await
            .expect("a lost race is a normal outcome");

        assert_eq!(applied, AppliedDecision::OwnershipLost);
        assert_eq!(store.released(), 0, "never release someone else's claim");
        assert!(
            store
                .labels_snapshot()
                .iter()
                .any(|l| l == "failure-count:1"),
            "accounting already applied before the race window"
        );
    }

    async fn retry_release_failure_is_reported_after_accounting() {
        let (_dir, workspace) = temp_workspace();
        let mut store = RecordingStore::new(workspace);
        store.fail_release = true;

        let result = apply(&executor(), &store, &retry_decision()).await;

        let error = result.expect_err("release failure must surface to the caller");
        assert!(format!("{error:#}").contains("failed to release bead"));
        assert!(store
            .labels_snapshot()
            .iter()
            .any(|label| label == "failure-count:1"));
        assert_eq!(store.released(), 0);
    }

    // ── blocked ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn blocked_records_prerequisite_and_blocks() {
        let (_dir, workspace) = temp_workspace();
        let store = RecordingStore::new(workspace);

        let applied = apply(&executor(), &store, &blocked_decision())
            .await
            .expect("blocked applies");

        assert_eq!(applied, AppliedDecision::Blocked);
        let notes = store.notes_snapshot();
        assert_eq!(notes.len(), 1, "one prerequisite note");
        assert!(
            notes[0].contains("external dependency"),
            "names the blocker type"
        );
        assert!(
            notes[0].contains("upstream service must publish schema v2"),
            "records the concrete prerequisite"
        );
        assert_eq!(store.blocks.load(Ordering::SeqCst), 1, "bead blocked");
        assert_eq!(store.released(), 0, "blocked, not released");
        assert!(
            !store
                .labels_snapshot()
                .iter()
                .any(|l| l.starts_with("failure-count:")),
            "blocking is not a failure — no penalty"
        );
    }

    #[tokio::test]
    async fn blocked_safely_releases_when_block_fails() {
        let (_dir, workspace) = temp_workspace();
        let mut store = RecordingStore::new(workspace);
        store.fail_block = true;

        let applied = apply(&executor(), &store, &blocked_decision())
            .await
            .expect("the safety release succeeded, so the outcome is terminal");

        assert_eq!(
            applied,
            AppliedDecision::Released(ReleaseCause::MutationFailed),
            "a failed block ends in the safety release"
        );
        assert_eq!(store.released(), 1, "the bead was not left in_progress");
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0);

        blocked_release_failure_is_reported_after_block_mutation_fails().await;
    }

    #[tokio::test]
    async fn blocked_race_ownership_lost_blocks_nothing() {
        let (_dir, workspace) = temp_workspace();
        let store = RecordingStore::new(workspace).flips_to_foreign_owner_after_n_reads(0);

        let applied = apply(&executor(), &store, &blocked_decision())
            .await
            .expect("a lost race is a normal outcome");

        assert_eq!(applied, AppliedDecision::OwnershipLost);
        assert!(store.notes_snapshot().is_empty());
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0);

        blocked_race_before_block_does_not_block_the_new_owner().await;
    }

    async fn blocked_race_before_block_does_not_block_the_new_owner() {
        let (_dir, workspace) = temp_workspace();
        // The prerequisite note is allowed to land while this worker still
        // owns the bead; the second ownership check must fence the state
        // transition itself after the handoff.
        let store = RecordingStore::new(workspace).flips_to_foreign_owner_after_n_reads(1);

        let applied = apply(&executor(), &store, &blocked_decision())
            .await
            .expect("a lost race is a normal outcome");

        assert_eq!(applied, AppliedDecision::OwnershipLost);
        assert_eq!(store.notes_snapshot().len(), 1, "prerequisite was recorded");
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0);
        assert_eq!(store.released(), 0, "never release the new owner's claim");
    }

    async fn blocked_release_failure_is_reported_after_block_mutation_fails() {
        let (_dir, workspace) = temp_workspace();
        let mut store = RecordingStore::new(workspace);
        store.fail_block = true;
        store.fail_release = true;

        let result = apply(&executor(), &store, &blocked_decision()).await;

        let error = result.expect_err("an unsafe release failure must surface");
        assert!(format!("{error:#}").contains("safety release failed"));
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0);
        assert_eq!(store.released(), 0);
    }

    // ── split ────────────────────────────────────────────────────────────

    fn executor_with_mitosis(lock_dir: &std::path::Path) -> DecisionExecutor {
        executor().with_mitosis(MitosisEvaluator::new(
            crate::config::MitosisConfig::default(),
            Telemetry::new("test".to_string()),
            lock_dir.to_path_buf(),
        ))
    }

    #[tokio::test]
    async fn split_creates_children_through_mitosis_and_blocks_parent() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        let store = RecordingStore::new(workspace);
        let executor = executor_with_mitosis(lock_dir.path());

        let applied = apply(
            &executor,
            &store,
            &split_decision("needle-exec", &["Add the parser", "Add the serializer"]),
        )
        .await
        .expect("split applies");

        match applied {
            AppliedDecision::Split { created, deduped } => {
                assert_eq!(created, 2, "both children created");
                assert_eq!(deduped, 0, "nothing deduped");
            }
            other => panic!("expected Split, got {other:?}"),
        }
        let children = store.children_snapshot();
        assert!(children.contains(&"Add the parser".to_string()));
        assert!(children.contains(&"Add the serializer".to_string()));
        assert_eq!(
            store.deps_snapshot(),
            vec![
                ("child-1".to_string(), "child-2".to_string()),
                ("child-2".to_string(), PARENT_ID.to_string()),
            ],
            "children chained sequentially, the parent depends on the last one"
        );
        let parent_labels = store.labels_snapshot();
        assert!(
            parent_labels.contains(&"umbrella".to_string())
                && parent_labels.contains(&"auto-split-parent".to_string()),
            "the parent was converted to an umbrella, got: {parent_labels:?}"
        );
        assert_eq!(
            store.blocks.load(Ordering::SeqCst),
            1,
            "parent blocked pending children"
        );
        assert_eq!(store.released(), 0);
    }

    #[tokio::test]
    async fn split_children_carry_the_split_child_label_chain_wide() {
        // The manual Auto-Split contract's exact labels: every child is a
        // `split-child` scoped to its parent — what Mend's orphaned-
        // split-child sweep and verified_completed_split's explicit
        // provenance proof both key on.
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        let store = RecordingStore::new(workspace);
        let executor = executor_with_mitosis(lock_dir.path());

        let applied = apply(
            &executor,
            &store,
            &split_decision(
                "needle-exec",
                &["Add the parser", "Add the serializer", "Add the CLI"],
            ),
        )
        .await
        .expect("split applies");

        assert!(
            matches!(applied, AppliedDecision::Split { created: 3, .. }),
            "three children created, got {applied:?}"
        );
        for child_id in ["child-1", "child-2", "child-3"] {
            let labels = store.child_labels(child_id);
            assert!(
                labels.contains(&"split-child".to_string()),
                "{child_id} carries split-child, got: {labels:?}"
            );
            assert!(
                labels.contains(&"parent-needle-exec".to_string()),
                "{child_id} carries the parent scope label, got: {labels:?}"
            );
        }
        assert_eq!(
            store.deps_snapshot(),
            vec![
                ("child-1".to_string(), "child-2".to_string()),
                ("child-2".to_string(), "child-3".to_string()),
                ("child-3".to_string(), PARENT_ID.to_string()),
            ],
            "a three-child chain with the parent on the terminal child only"
        );
    }

    fn existing_parser_child() -> Bead {
        Bead {
            id: BeadId::from("needle-existing"),
            title: "Add the parser".to_string(),
            body: Some("Created by an earlier split".to_string()),
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: vec![
                "mitosis-child".to_string(),
                "parent-needle-exec".to_string(),
                "root-needle-exec".to_string(),
            ],
            workspace: PathBuf::new(),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn split_dedupes_against_existing_children() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        // An existing child of this parent already covers one of the two
        // proposals; only the uncovered one may be created.
        let store = RecordingStore::new(workspace).with_existing_child(existing_parser_child());

        let applied = {
            let bead = store.bead();
            executor_with_mitosis(lock_dir.path())
                .apply(
                    &store,
                    &bead,
                    &split_decision("needle-exec", &["Add the parser", "Add the serializer"]),
                    "worker-a",
                    None,
                )
                .await
                .expect("split with partial dedup applies")
        };

        match applied {
            AppliedDecision::Split { created, deduped } => {
                assert_eq!(created, 1, "only the uncovered child is created");
                assert_eq!(deduped, 1, "the covered proposal is deduped");
            }
            other => panic!("expected Split, got {other:?}"),
        }
        let children = store.children_snapshot();
        assert_eq!(children, vec!["Add the serializer".to_string()]);
        assert_eq!(
            store.dependencies.lock().unwrap().len(),
            1,
            "the created child blocks the parent"
        );
        assert_eq!(
            store.blocks.load(Ordering::SeqCst),
            1,
            "parent blocked pending children"
        );
    }

    #[tokio::test]
    async fn split_fully_deduplicated_releases_the_parent() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        // Every proposed child already exists — nothing to create, so the
        // parent returns to the frontier instead of being blocked forever.
        let store = RecordingStore::new(workspace).with_existing_child(existing_parser_child());
        let executor = executor_with_mitosis(lock_dir.path());

        let applied = apply(
            &executor,
            &store,
            &split_decision("needle-exec", &["Add the parser"]),
        )
        .await
        .expect("a fully covered proposal resolves without error");

        assert_eq!(
            applied,
            AppliedDecision::Released(ReleaseCause::SplitCovered),
            "nothing new to create — the parent goes back to the frontier"
        );
        assert!(store.children_snapshot().is_empty(), "no child created");
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0, "parent not blocked");
        assert_eq!(store.released(), 1);

        split_fully_deduplicated_race_does_not_release_after_handoff().await;
    }

    async fn split_fully_deduplicated_race_does_not_release_after_handoff() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        // Reads: entry, pre-creation, then the release guard after Mitosis
        // proves that every proposal is already covered.
        let store = RecordingStore::new(workspace)
            .with_existing_child(existing_parser_child())
            .flips_to_foreign_owner_after_n_reads(2);
        let executor = executor_with_mitosis(lock_dir.path());

        let applied = apply(
            &executor,
            &store,
            &split_decision("needle-exec", &["Add the parser"]),
        )
        .await
        .expect("a lost race is a normal outcome");

        assert_eq!(applied, AppliedDecision::OwnershipLost);
        assert!(store.children_snapshot().is_empty());
        assert_eq!(store.released(), 0, "never release the new owner's claim");
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn split_deduplicates_duplicate_titles_within_a_proposal() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        // Two identical proposals with no existing children: one child is
        // created, the duplicate proposal is counted as deduped.
        let store = RecordingStore::new(workspace);
        let executor = executor_with_mitosis(lock_dir.path());

        let applied = apply(
            &executor,
            &store,
            &split_decision("needle-exec", &["Same task", "Same task"]),
        )
        .await
        .expect("a proposal with duplicate titles applies once");

        match applied {
            AppliedDecision::Split { created, deduped } => {
                assert_eq!(created, 1, "identical titles create one child");
                assert_eq!(deduped, 1, "the duplicate title is deduped");
            }
            other => panic!("expected Split, got {other:?}"),
        }
        assert_eq!(store.children_snapshot(), vec!["Same task".to_string()]);
    }

    #[tokio::test]
    async fn split_refuses_a_decision_that_names_a_different_parent() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        let store = RecordingStore::new(workspace);
        let executor = executor_with_mitosis(lock_dir.path());

        let result = apply(
            &executor,
            &store,
            &split_decision("needle-other", &["Add the parser"]),
        )
        .await;

        let error = result.expect_err("a split naming a foreign parent is refused");
        assert!(
            format!("{error:#}").contains("names parent needle-other"),
            "the error names the mismatch, got: {error:#}"
        );
        assert!(store.children_snapshot().is_empty(), "no child created");
        assert!(store.closes_snapshot().is_empty());
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0);
        assert_eq!(
            store.released(),
            1,
            "the resolution_failed fallback released it"
        );
    }

    #[tokio::test]
    async fn split_without_a_mitosis_evaluator_is_refused_and_released() {
        let (_dir, workspace) = temp_workspace();
        let store = RecordingStore::new(workspace);

        let result = apply(
            &executor(),
            &store,
            &split_decision("needle-exec", &["Add the parser"]),
        )
        .await;

        let error = result.expect_err("split proposals are never created raw");
        assert!(
            format!("{error:#}").contains("never created raw"),
            "the error explains the refusal, got: {error:#}"
        );
        assert!(store.children_snapshot().is_empty());
        assert_eq!(
            store.released(),
            1,
            "safely released, never left in_progress"
        );

        split_with_empty_child_title_is_rejected_before_mitosis().await;
    }

    #[tokio::test]
    async fn split_refused_by_mitosis_creates_nothing_and_releases() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        // Entry validation passed the decision through, but the mitosis
        // boundary refuses it anyway: a max_children cap of 0 leaves nothing
        // to create. The executor's Refused arm must release the parent
        // without creating a child, blocking, or labelling anything.
        let executor = executor().with_mitosis(MitosisEvaluator::new(
            crate::config::MitosisConfig {
                max_children: 0,
                ..Default::default()
            },
            Telemetry::new("test".to_string()),
            lock_dir.path().to_path_buf(),
        ));
        let store = RecordingStore::new(workspace);

        let result = apply(
            &executor,
            &store,
            &split_decision("needle-exec", &["Add the parser", "Add the serializer"]),
        )
        .await;

        let error = result.expect_err("a refused split is a resolution failure");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("split proposal refused"),
            "the error explains the refusal, got: {rendered}"
        );
        assert!(
            rendered.contains("max_children cap (0)"),
            "the refusal names the cap that caused it, got: {rendered}"
        );
        assert!(
            store.children_snapshot().is_empty(),
            "a refused proposal creates nothing"
        );
        assert!(
            !store
                .labels_snapshot()
                .iter()
                .any(|l| l == "umbrella" || l == "auto-split-parent"),
            "a refused proposal never converts the parent, got: {:?}",
            store.labels_snapshot()
        );
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0, "nothing blocked");
        assert!(store.closes_snapshot().is_empty(), "nothing closed");
        assert_eq!(
            store.released(),
            1,
            "safely released, never left in_progress"
        );
    }

    async fn split_with_empty_child_title_is_rejected_before_mitosis() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        let store = RecordingStore::new(workspace);
        let executor = executor_with_mitosis(lock_dir.path());

        let result = apply(&executor, &store, &split_decision("needle-exec", &["   "])).await;

        let error = result.expect_err("malformed split must be refused");
        assert!(format!("{error:#}").contains("validation"));
        assert!(store.children_snapshot().is_empty());
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0);
        assert_eq!(store.released(), 1, "refusal safely releases the claim");
    }

    #[tokio::test]
    async fn split_ownership_lost_before_creation_creates_nothing() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        // Read 0 is the entry check; read 1 is the re-check immediately
        // before children start being created. The bead is reassigned in
        // between: the losing split must not create a single child.
        let store = RecordingStore::new(workspace).flips_to_foreign_owner_after_n_reads(1);
        let executor = executor_with_mitosis(lock_dir.path());

        let applied = apply(
            &executor,
            &store,
            &split_decision(
                "needle-exec",
                &["Add the parser", "Add the serializer", "Add the CLI"],
            ),
        )
        .await
        .expect("a lost race is a normal outcome");

        assert_eq!(applied, AppliedDecision::OwnershipLost);
        assert!(
            store.children_snapshot().is_empty(),
            "a stale dispatch must not create children"
        );
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0, "nothing blocked");
        assert_eq!(store.released(), 0, "never release someone else's claim");

        split_race_before_parent_block_does_not_block_the_new_owner().await;
    }

    async fn split_race_before_parent_block_does_not_block_the_new_owner() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        // Mitosis has created and wired the children. The winner closes the
        // parent before the executor's final ownership guard, so the losing
        // split must not block or release that new owner.
        let store = RecordingStore::new(workspace).closed_by_winner_after_n_reads(2);
        let executor = executor_with_mitosis(lock_dir.path());

        let applied = apply(
            &executor,
            &store,
            &split_decision("needle-exec", &["Add the parser", "Add the serializer"]),
        )
        .await
        .expect("a lost race is a normal outcome");

        assert_eq!(applied, AppliedDecision::OwnershipLost);
        assert_eq!(store.children_snapshot().len(), 2);
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0);
        assert_eq!(store.released(), 0, "never release the new owner's claim");
    }

    #[tokio::test]
    async fn split_creation_failure_compensates_and_releases_the_parent() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        // The second create fails: child-1 exists with no chain edge yet.
        // The aborted split must close child-1 and the executor must release
        // the parent — no orphaned half-split state anywhere.
        let store = RecordingStore::new(workspace).failing_create_after(1);
        let executor = executor_with_mitosis(lock_dir.path());

        let applied = apply(
            &executor,
            &store,
            &split_decision(
                "needle-exec",
                &["Add the parser", "Add the serializer", "Add the CLI"],
            ),
        )
        .await
        .expect("the compensation succeeded, so the outcome is terminal");

        assert_eq!(
            applied,
            AppliedDecision::Released(ReleaseCause::MutationFailed),
            "a mid-creation failure ends in the safety release"
        );
        assert_eq!(
            store.children_snapshot().len(),
            1,
            "only the first child was created before the failure"
        );
        assert_eq!(
            store.child_close_reasons().len(),
            1,
            "the created child was compensated"
        );
        assert!(
            store.open_child_ids().is_empty(),
            "no orphaned half-split children remain open"
        );
        assert_eq!(store.released(), 1, "the parent was not left in_progress");
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0, "nothing blocked");
        assert!(
            !store
                .labels_snapshot()
                .iter()
                .any(|l| l == "umbrella" || l == "auto-split-parent"),
            "an aborted split does not convert the parent to an umbrella"
        );
    }

    #[tokio::test]
    async fn split_chain_failure_compensates_the_whole_partial_chain() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        // The first chain edge (child-1 -> child-2) fails after both children
        // were created: both must be compensated.
        let store = RecordingStore::new(workspace).arm_failure("dep");
        let executor = executor_with_mitosis(lock_dir.path());

        let applied = apply(
            &executor,
            &store,
            &split_decision(
                "needle-exec",
                &["Add the parser", "Add the serializer", "Add the CLI"],
            ),
        )
        .await
        .expect("the compensation succeeded, so the outcome is terminal");

        assert_eq!(
            applied,
            AppliedDecision::Released(ReleaseCause::MutationFailed)
        );
        assert_eq!(store.children_snapshot().len(), 2);
        assert_eq!(
            store.child_close_reasons().len(),
            2,
            "both children of the partial chain were compensated"
        );
        assert!(store.open_child_ids().is_empty());
        assert!(store.deps_snapshot().is_empty(), "no edge was committed");
        assert_eq!(store.released(), 1);
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn split_umbrella_label_failure_compensates_after_full_wiring() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        // Everything wires, then the first umbrella label fails. The split is
        // still aborted: without both labels the parent would be an
        // ordinary bead wearing an unexplained chain, so the children go.
        let store = RecordingStore::new(workspace).arm_failure("label");
        let executor = executor_with_mitosis(lock_dir.path());

        let applied = apply(
            &executor,
            &store,
            &split_decision(
                "needle-exec",
                &["Add the parser", "Add the serializer", "Add the CLI"],
            ),
        )
        .await
        .expect("the compensation succeeded, so the outcome is terminal");

        assert_eq!(
            applied,
            AppliedDecision::Released(ReleaseCause::MutationFailed)
        );
        assert_eq!(store.deps_snapshot().len(), 3, "the chain itself wired");
        assert_eq!(
            store.child_close_reasons().len(),
            3,
            "all three children were compensated"
        );
        assert!(store.open_child_ids().is_empty());
        assert!(
            !store
                .labels_snapshot()
                .iter()
                .any(|l| l == "umbrella" || l == "auto-split-parent"),
            "a half-labelled parent is not left behind"
        );
        assert_eq!(store.released(), 1);
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn split_partial_umbrella_conversion_is_rolled_back() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        // `umbrella` lands, then `auto-split-parent` fails. The compensation
        // must take the applied label back off: an open parent wearing
        // `umbrella` and depending on its (now closed) last child is the
        // exact shape `verified_completed_split` walks — through the
        // non-explicit path (no auto-split-parent label, failure-count label
        // present) it would close the parent as a *completed* split whose
        // work never happened.
        let store = RecordingStore::new(workspace).failing_parent_label_after(1);
        let executor = executor_with_mitosis(lock_dir.path());

        let applied = apply(
            &executor,
            &store,
            &split_decision(
                "needle-exec",
                &["Add the parser", "Add the serializer", "Add the CLI"],
            ),
        )
        .await
        .expect("the compensation succeeded, so the outcome is terminal");

        assert_eq!(
            applied,
            AppliedDecision::Released(ReleaseCause::MutationFailed)
        );
        assert!(
            !store
                .labels_snapshot()
                .iter()
                .any(|l| l == "umbrella" || l == "auto-split-parent"),
            "the applied umbrella label was rolled back, got {:?}",
            store.labels_snapshot()
        );
        assert_eq!(
            store.child_close_reasons().len(),
            3,
            "all three children were compensated"
        );
        assert!(store.open_child_ids().is_empty());
        assert_eq!(store.released(), 1);
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0);
    }

    // ── cross-decision races ─────────────────────────────────────────────

    #[tokio::test]
    async fn complete_loses_to_a_won_split_and_mutates_nothing() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        let store = RecordingStore::new(workspace);
        let executor = executor_with_mitosis(lock_dir.path());

        // The split decision wins: children created, parent blocked.
        let won = apply(
            &executor,
            &store,
            &split_decision("needle-exec", &["Add the parser", "Add the serializer"]),
        )
        .await
        .expect("the winning split applies");
        assert!(matches!(won, AppliedDecision::Split { .. }));

        // A stale complete dispatch — still holding the pre-split snapshot —
        // now applies. The bead is Deferred (and unowned in the real store);
        // exactly one decision may mutate it: the complete must lose without
        // closing or releasing anything.
        let stale_bead = store.bead();
        let applied = executor
            .apply(&store, &stale_bead, &complete_decision(), "worker-a", None)
            .await
            .expect("a lost race is a normal outcome, not an error");

        assert_eq!(applied, AppliedDecision::OwnershipLost);
        assert!(
            store.closes_snapshot().is_empty(),
            "the loser must not close a bead the winner blocked"
        );
        assert_eq!(
            store.released(),
            0,
            "the loser releases safely: never someone else's claim"
        );
        assert_eq!(
            store.children_snapshot().len(),
            2,
            "the winner's children stand untouched"
        );
    }

    #[tokio::test]
    async fn split_loses_to_a_won_complete_and_creates_nothing() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        // The complete decision wins mid-flight: the bead is closed (Done) by
        // its owner exactly at the split's pre-creation re-check (read 1).
        let store = RecordingStore::new(workspace).closed_by_winner_after_n_reads(1);
        let executor = executor_with_mitosis(lock_dir.path());

        let applied = apply(
            &executor,
            &store,
            &split_decision("needle-exec", &["Add the parser", "Add the serializer"]),
        )
        .await
        .expect("a lost race is a normal outcome, not an error");

        assert_eq!(applied, AppliedDecision::OwnershipLost);
        assert!(
            store.children_snapshot().is_empty(),
            "the losing split creates no children on a closed bead"
        );
        assert_eq!(store.blocks.load(Ordering::SeqCst), 0);
        assert_eq!(store.released(), 0, "never release someone else's claim");
        assert_eq!(
            store.closes_snapshot().len(),
            0,
            "the split did not close anything either"
        );
    }

    #[tokio::test]
    async fn retry_loses_to_a_won_split_and_touches_nothing() {
        let (_dir, workspace) = temp_workspace();
        let lock_dir = tempfile::tempdir().expect("lock tempdir");
        let store = RecordingStore::new(workspace);
        let executor = executor_with_mitosis(lock_dir.path());

        // The split wins: the parent is blocked pending its chain.
        let won = apply(
            &executor,
            &store,
            &split_decision("needle-exec", &["Add the parser", "Add the serializer"]),
        )
        .await
        .expect("the winning split applies");
        assert!(matches!(won, AppliedDecision::Split { .. }));

        // A stale retry dispatch applies against the post-split store: it
        // must lose at the entry ownership check without recording guidance,
        // incrementing failure accounting, or releasing.
        let stale_bead = store.bead();
        let applied = executor
            .apply(&store, &stale_bead, &retry_decision(), "worker-a", None)
            .await
            .expect("a lost race is a normal outcome, not an error");

        assert_eq!(applied, AppliedDecision::OwnershipLost);
        assert!(
            store.notes_snapshot().is_empty(),
            "no guidance note on a bead the winner blocked"
        );
        assert!(
            !store
                .labels_snapshot()
                .iter()
                .any(|l| l.starts_with("failure-count:")),
            "no failure accounting on a foreign claim"
        );
        assert_eq!(store.released(), 0, "never release someone else's claim");
    }

    // ── shared contract ──────────────────────────────────────────────────

    #[tokio::test]
    async fn invalid_decisions_are_refused_without_mutation() {
        let (_dir, workspace) = temp_workspace();
        let store = RecordingStore::new(workspace);
        let invalid = ResolveDecision::Complete {
            evidence: String::new(),
            commit_message: "fix: the thing".to_string(),
        };

        let result = apply(&executor(), &store, &invalid).await;

        let error = result.expect_err("an unvalidated decision must not drive mutations");
        assert!(
            format!("{error:#}").contains("validation"),
            "the error names validation, got: {error:#}"
        );
        assert!(store.closes_snapshot().is_empty());
        assert_eq!(
            store.released(),
            1,
            "the resolution_failed fallback released it"
        );
    }

    #[test]
    fn concise_collapses_whitespace_and_caps_length() {
        assert_eq!(concise("a\n b\t c", 20), "a b c");
        let long = concise("x".repeat(100).as_str(), 10);
        assert_eq!(long.chars().count(), 10);
        assert!(long.ends_with('…'));
        assert_eq!(concise("short", 20), "short");
    }
}
