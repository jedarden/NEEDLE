//! OpenTelemetry trace span definitions and helpers.
//!
//! This module provides span names, attribute keys, and helper functions
//! for creating OTel-compliant spans throughout the NEEDLE state machine.
//!
//! ## Span Hierarchy
//!
//! ```text
//! worker.session                                          (root span, lifetime = worker process)
//! ├── strand.pluck                                        (one per strand evaluation)
//! │   └── bead.lifecycle                                  (one per claimed bead)
//! │       ├── bead.claim                                  (ATOMIC phase)
//! │       ├── bead.prompt_build
//! │       ├── agent.dispatch                              (DISPATCHING + EXECUTING)
//! │       │   └── agent.execution                         (process alive; span.ok on exit 0)
//! │       └── bead.outcome                                (HANDLING)
//! │           └── bead.mitosis?                           (optional, if outcome = failure)
//! ├── strand.mend
//! ├── strand.explore
//! ├── strand.weave
//! ├── strand.unravel
//! ├── strand.pulse
//! └── strand.knot                                         (terminal backoff / exhaustion)
//! ```

use tracing::{error, Span};

/// Span names following OTel conventions (lowercase dotted).
pub mod span_names {
    pub const WORKER_SESSION: &str = "worker.session";
    pub const STRAND_PREFIX: &str = "strand";

    pub fn strand(strand_name: &str) -> String {
        format!("{}.{}", STRAND_PREFIX, strand_name)
    }

    pub const BEAD_LIFECYCLE: &str = "bead.lifecycle";
    pub const BEAD_CLAIM: &str = "bead.claim";
    pub const BEAD_PROMPT_BUILD: &str = "bead.prompt_build";
    pub const AGENT_DISPATCH: &str = "agent.dispatch";
    pub const AGENT_EXECUTION: &str = "agent.execution";
    pub const BEAD_OUTCOME: &str = "bead.outcome";
    pub const BEAD_MITOSIS: &str = "bead.mitosis";
}

/// Attribute keys following OTel semantic conventions.
pub mod attrs {
    // Worker session attributes
    pub const NEEDLE_BEADS_PROCESSED: &str = "needle.beads_processed";
    pub const NEEDLE_UPTIME_SECONDS: &str = "needle.uptime_seconds";
    pub const NEEDLE_EXIT_REASON: &str = "needle.exit_reason";

    // Strand attributes
    pub const NEEDLE_STRAND_NAME: &str = "needle.strand.name";
    pub const NEEDLE_STRAND_RESULT: &str = "needle.strand.result";
    pub const NEEDLE_STRAND_DURATION_MS: &str = "needle.strand.duration_ms";

    // Bead attributes
    pub const NEEDLE_BEAD_ID: &str = "needle.bead.id";
    pub const NEEDLE_BEAD_PRIORITY: &str = "needle.bead.priority";
    pub const NEEDLE_BEAD_TITLE_HASH: &str = "needle.bead.title_hash";
    pub const NEEDLE_BEAD_OUTCOME: &str = "needle.bead.outcome";

    // Claim attributes
    pub const NEEDLE_CLAIM_RETRY_NUMBER: &str = "needle.claim.retry_number";
    pub const NEEDLE_CLAIM_RESULT: &str = "needle.claim.result";

    // Agent attributes (gen_ai semantic conventions)
    pub const GEN_AI_SYSTEM: &str = "gen_ai.system";
    pub const GEN_AI_OPERATION_NAME: &str = "gen_ai.operation.name";
    pub const GEN_AI_REQUEST_MODEL: &str = "gen_ai.request.model";
    pub const GEN_AI_USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
    pub const GEN_AI_USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";
    pub const NEEDLE_AGENT_PID: &str = "needle.agent.pid";
    pub const NEEDLE_AGENT_EXIT_CODE: &str = "needle.agent.exit_code";

    // Outcome attributes
    pub const NEEDLE_OUTCOME: &str = "needle.outcome";
    pub const NEEDLE_OUTCOME_ACTION: &str = "needle.outcome.action";
}

/// Strand result values for telemetry.
pub mod strand_results {
    pub const BEAD_FOUND: &str = "bead_found";
    pub const WORK_CREATED: &str = "work_created";
    pub const NO_WORK: &str = "no_work";
    pub const ERROR: &str = "error";
}

/// Claim result values for telemetry.
pub mod claim_results {
    pub const SUCCEEDED: &str = "succeeded";
    pub const RACE_LOST: &str = "race_lost";
    pub const FAILED: &str = "failed";
}

/// Outcome values for telemetry.
pub mod outcomes {
    pub const SUCCESS: &str = "success";
    pub const FAILURE: &str = "failure";
    pub const TIMEOUT: &str = "timeout";
    pub const CRASH: &str = "crash";
    pub const AGENT_NOT_FOUND: &str = "agent_not_found";
    pub const INTERRUPTED: &str = "interrupted";
}

/// Record an error on a span with a description.
///
/// This sets the span status to Error with the given description,
/// following OTel conventions for error spans.
pub fn record_span_error(span: &Span, description: &str) {
    span.record("error", description);
    error!(parent: span, "{}", description);
}

/// Record an outcome on a span.
///
/// Sets the outcome attribute and, if the outcome is not success,
/// marks the span as errored.
pub fn record_outcome(span: &Span, outcome: &str) {
    span.record(attrs::NEEDLE_BEAD_OUTCOME, outcome);
    if outcome != outcomes::SUCCESS {
        record_span_error(span, outcome);
    }
}

/// Record an outcome action on a span.
pub fn record_outcome_action(span: &Span, action: &str) {
    span.record(attrs::NEEDLE_OUTCOME_ACTION, action);
}

/// Record strand result on a span.
pub fn record_strand_result(span: &Span, result: &str) {
    span.record(attrs::NEEDLE_STRAND_RESULT, result);
}

/// Record claim result on a span.
pub fn record_claim_result(span: &Span, result: &str) {
    span.record(attrs::NEEDLE_CLAIM_RESULT, result);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_names_are_dotted_lowercase() {
        assert_eq!(span_names::WORKER_SESSION, "worker.session");
        assert_eq!(span_names::BEAD_LIFECYCLE, "bead.lifecycle");
        assert_eq!(span_names::BEAD_CLAIM, "bead.claim");
        assert_eq!(span_names::BEAD_PROMPT_BUILD, "bead.prompt_build");
        assert_eq!(span_names::AGENT_DISPATCH, "agent.dispatch");
        assert_eq!(span_names::AGENT_EXECUTION, "agent.execution");
        assert_eq!(span_names::BEAD_OUTCOME, "bead.outcome");
        assert_eq!(span_names::BEAD_MITOSIS, "bead.mitosis");
    }

    #[test]
    fn strand_name_builder() {
        assert_eq!(span_names::strand("pluck"), "strand.pluck");
        assert_eq!(span_names::strand("mend"), "strand.mend");
        assert_eq!(span_names::strand("explore"), "strand.explore");
    }

    #[test]
    fn attribute_keys_follow_conventions() {
        // Worker session
        assert_eq!(attrs::NEEDLE_BEADS_PROCESSED, "needle.beads_processed");
        assert_eq!(attrs::NEEDLE_UPTIME_SECONDS, "needle.uptime_seconds");
        assert_eq!(attrs::NEEDLE_EXIT_REASON, "needle.exit_reason");

        // Strand
        assert_eq!(attrs::NEEDLE_STRAND_NAME, "needle.strand.name");
        assert_eq!(attrs::NEEDLE_STRAND_RESULT, "needle.strand.result");

        // Bead
        assert_eq!(attrs::NEEDLE_BEAD_ID, "needle.bead.id");
        assert_eq!(attrs::NEEDLE_BEAD_PRIORITY, "needle.bead.priority");

        // Agent (gen_ai semantic conventions)
        assert_eq!(attrs::GEN_AI_OPERATION_NAME, "gen_ai.operation.name");
        assert_eq!(attrs::GEN_AI_SYSTEM, "gen_ai.system");
        assert_eq!(attrs::GEN_AI_REQUEST_MODEL, "gen_ai.request.model");
        assert_eq!(
            attrs::GEN_AI_USAGE_INPUT_TOKENS,
            "gen_ai.usage.input_tokens"
        );
    }

    #[test]
    fn result_values_match_expected() {
        assert_eq!(strand_results::BEAD_FOUND, "bead_found");
        assert_eq!(strand_results::WORK_CREATED, "work_created");
        assert_eq!(strand_results::NO_WORK, "no_work");
        assert_eq!(strand_results::ERROR, "error");

        assert_eq!(claim_results::SUCCEEDED, "succeeded");
        assert_eq!(claim_results::RACE_LOST, "race_lost");
        assert_eq!(claim_results::FAILED, "failed");

        assert_eq!(outcomes::SUCCESS, "success");
        assert_eq!(outcomes::FAILURE, "failure");
        assert_eq!(outcomes::TIMEOUT, "timeout");
        assert_eq!(outcomes::CRASH, "crash");
        assert_eq!(outcomes::AGENT_NOT_FOUND, "agent_not_found");
        assert_eq!(outcomes::INTERRUPTED, "interrupted");
    }
}
