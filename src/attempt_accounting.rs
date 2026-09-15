//! Complete attempt accounting (ADR-030, plan section 4.9).
//!
//! The `attempt.resolved` ledger is the evidence every learning consumer
//! reads: `needle stats`, evidence routing, prompt-variant canaries, and the
//! ADR-029 proposal generator after them. The classification rules that keep
//! that evidence honest live here as pure functions. The outcome handler
//! applies them to the row it emits and consumers read the resulting class,
//! so no consumer re-derives a rule and a new consumer cannot get one wrong
//! (the alternative ADR-030 rejected).
//!
//! # N-T46: a decomposition is not a success
//!
//! When an attempt splits its bead into children instead of delivering the
//! work, its row resolves [`DECOMPOSED`]. It earns no verified credit and is
//! no failure; delivered work on the children earns credit on the children's
//! own attempts. Before this class existed the auto-split template's
//! successes were booked as `verified_success`, which is why verified yield
//! read 86% on fourth attempts and 97% on fifth-or-later attempts in the
//! 2026-09-12..14 ledger (plan section 4.9).

use crate::mitosis::AUTO_SPLIT_PARENT_LABEL;

/// Wire outcome of an attempt that decomposed its bead instead of delivering
/// it: the `decomposed` variant of [`crate::telemetry::AttemptOutcome`].
pub const DECOMPOSED: &str = "decomposed";

/// Prompt template the worker dispatches once a bead reaches its auto-split
/// threshold (`strands.pluck.split_after_failures`).
pub const SPLIT_TEMPLATE: &str = "split";

const VERIFIED_SUCCESS: &str = "verified_success";

/// Outcome recorded in the bead backend for a decomposed attempt.
///
/// bead-rs's attempt-outcome-v1 vocabulary has no decomposition class and
/// rejects outcomes it does not know. `indeterminate` is the member with the
/// right effect there: it neither resets nor extends the bead's
/// consecutive-failure run and does not move its attempt tier. The row's
/// `decomposed:…` terminal reason travels with it as the resolution reason.
const BACKEND_DECOMPOSED: &str = "indeterminate";

/// Why an attempt resolved [`DECOMPOSED`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decomposition {
    /// The dispatch itself asked for a split: the auto-split template.
    SplitTemplate,
    /// The attempt made its bead an auto-split parent and delivered no commit.
    SplitParentWithoutCommits,
}

impl Decomposition {
    /// The ledger row's `terminal_reason`.
    pub fn terminal_reason(self) -> &'static str {
        match self {
            Decomposition::SplitTemplate => "decomposed:split_template",
            Decomposition::SplitParentWithoutCommits => "decomposed:split_parent_without_commits",
        }
    }
}

/// The facts about one resolved attempt that decide whether it decomposed.
#[derive(Debug, Clone, Copy)]
pub struct AttemptShape<'a> {
    /// Semantic outcome the attempt would otherwise resolve with.
    pub outcome: &'a str,
    /// Prompt template that built the dispatch prompt.
    pub prompt_template: &'a str,
    /// Whether the attempt created any commit in its workspace.
    pub delivered_commits: bool,
    /// The bead's labels when it was dispatched.
    pub labels_before: &'a [String],
    /// The bead's labels after the attempt, when they were read back.
    pub labels_after: Option<&'a [String]>,
}

/// Whether classifying an attempt needs the bead's post-attempt labels.
///
/// Only a commit-less verified success outside the split template can be a
/// decomposition the template name does not already reveal, so only that
/// shape pays for a backend read.
pub fn needs_labels_after(outcome: &str, prompt_template: &str, delivered_commits: bool) -> bool {
    outcome == VERIFIED_SUCCESS && prompt_template != SPLIT_TEMPLATE && !delivered_commits
}

/// Decide whether an attempt decomposed its bead (ADR-030 decision 1).
///
/// Only an attempt that would otherwise earn verified credit is
/// reclassified. A split attempt that failed or timed out already earns
/// none, and its failure evidence is what retries and routing need.
pub fn classify_decomposition(shape: &AttemptShape<'_>) -> Option<Decomposition> {
    if shape.outcome != VERIFIED_SUCCESS {
        return None;
    }
    if shape.prompt_template == SPLIT_TEMPLATE {
        return Some(Decomposition::SplitTemplate);
    }
    let is_split_parent = |labels: &[String]| labels.iter().any(|l| l == AUTO_SPLIT_PARENT_LABEL);
    let became_split_parent = shape
        .labels_after
        .is_some_and(|after| is_split_parent(after) && !is_split_parent(shape.labels_before));
    (became_split_parent && !shape.delivered_commits)
        .then_some(Decomposition::SplitParentWithoutCommits)
}

/// The outcome to record in the bead backend's own attempt ledger for a
/// NEEDLE ledger outcome. Every outcome but [`DECOMPOSED`] passes through.
pub fn backend_outcome(ledger_outcome: &str) -> &str {
    if ledger_outcome == DECOMPOSED {
        BACKEND_DECOMPOSED
    } else {
        ledger_outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn shape<'a>(
        outcome: &'a str,
        prompt_template: &'a str,
        delivered_commits: bool,
        before: &'a [String],
        after: Option<&'a [String]>,
    ) -> AttemptShape<'a> {
        AttemptShape {
            outcome,
            prompt_template,
            delivered_commits,
            labels_before: before,
            labels_after: after,
        }
    }

    #[test]
    fn nt46_decomposed_is_the_attempt_outcome_wire_string() {
        assert_eq!(
            DECOMPOSED,
            crate::telemetry::AttemptOutcome::Decomposed.as_str()
        );
    }

    #[test]
    fn nt46_a_split_template_success_is_decomposed_with_or_without_commits() {
        for delivered_commits in [false, true] {
            assert_eq!(
                classify_decomposition(&shape(
                    "verified_success",
                    "split",
                    delivered_commits,
                    &[],
                    None
                )),
                Some(Decomposition::SplitTemplate)
            );
        }
        assert_eq!(
            Decomposition::SplitTemplate.terminal_reason(),
            "decomposed:split_template"
        );
    }

    #[test]
    fn nt46_a_split_attempt_that_earned_no_credit_keeps_its_outcome() {
        for outcome in [
            "work_failure",
            "indeterminate",
            "infrastructure_failure",
            "cancelled",
        ] {
            assert_eq!(
                classify_decomposition(&shape(outcome, "split", false, &[], None)),
                None,
                "{outcome} must stay {outcome}"
            );
        }
    }

    #[test]
    fn nt46_a_new_split_parent_without_commits_is_decomposed() {
        let none = labels(&[]);
        let parent = labels(&["umbrella", AUTO_SPLIT_PARENT_LABEL]);
        assert_eq!(
            classify_decomposition(&shape(
                "verified_success",
                "pluck",
                false,
                &none,
                Some(&parent)
            )),
            Some(Decomposition::SplitParentWithoutCommits)
        );
        // A delivered commit is an artifact: the attempt did work.
        assert_eq!(
            classify_decomposition(&shape(
                "verified_success",
                "pluck",
                true,
                &none,
                Some(&parent)
            )),
            None
        );
        // A bead that was already a split parent was not split by this attempt.
        assert_eq!(
            classify_decomposition(&shape(
                "verified_success",
                "pluck",
                false,
                &parent,
                Some(&parent)
            )),
            None
        );
        // Labels that were never read back decide nothing.
        assert_eq!(
            classify_decomposition(&shape("verified_success", "pluck", false, &none, None)),
            None
        );
    }

    #[test]
    fn nt46_only_a_commit_less_verified_success_outside_split_needs_a_read() {
        assert!(needs_labels_after("verified_success", "pluck", false));
        assert!(!needs_labels_after("verified_success", "pluck", true));
        assert!(!needs_labels_after("verified_success", "split", false));
        assert!(!needs_labels_after("work_failure", "pluck", false));
    }

    #[test]
    fn nt46_the_backend_records_decomposed_as_indeterminate() {
        assert_eq!(backend_outcome("decomposed"), "indeterminate");
        for outcome in [
            "verified_success",
            "work_failure",
            "infrastructure_failure",
            "cancelled",
            "indeterminate",
        ] {
            assert_eq!(backend_outcome(outcome), outcome);
        }
    }
}
