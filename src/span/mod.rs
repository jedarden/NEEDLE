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
    pub const SKIPPED: &str = "skipped";
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

/// RAII guard for LIFO-safe span scoping.
///
/// When dropped, the span is explicitly closed, ensuring proper unwinding
/// even across early returns or errors. This ensures LIFO (Last-In-First-Out)
/// compliance by using RAII to guarantee spans unwind in reverse order.
///
/// # Safety
///
/// This guard must NOT be stored across an `.await` point. The guard should
/// only be used for synchronous code blocks. For async code, use
/// `.instrument()` on the future instead.
///
/// # Example
///
/// ```rust
/// let _guard = ScopeGuard::new(lifecycle_span.clone());
/// do_synchronous_work();
/// // Guard dropped here, span unwinds
/// ```
pub struct ScopeGuard {
    _guard: Option<tracing::span::EnteredSpan>,
}

impl ScopeGuard {
    /// Create a new scope guard from a span, entering it immediately.
    ///
    /// The span will be exited when the guard is dropped (LIFO order).
    ///
    /// # Panics
    ///
    /// This function uses `span.entered()` which modifies thread-local state.
    /// Do not store the returned guard across an `.await` point.
    pub fn new(span: Span) -> Self {
        // `Span::entered` consumes the span and returns an `EnteredSpan` that
        // OWNS it, so there is no borrow to outlive. The previous version
        // entered a by-value local and transmuted the resulting
        // `Entered<'_>` to `Entered<'static>`; the borrowed `Span` was then
        // dropped at the end of this function, leaving the guard holding a
        // dangling `&Span` that `Entered::drop` dereferenced to call `exit()`
        // -- a null vtable dispatch (segfault at ip=0).
        Self {
            _guard: Some(span.entered()),
        }
    }

    /// Enter the span scope for a synchronous block.
    ///
    /// This method is called automatically by `new()`, but is also available
    /// for cases where manual scope control is needed.
    ///
    /// # Example
    ///
    /// ```rust
    /// let lifecycle_guard = ScopeGuard::new(lifecycle_span.clone());
    /// lifecycle_guard.enter_scope(|| {
    ///     // This code runs within the lifecycle span
    ///     do_work();
    /// });
    /// ```
    pub fn enter_scope(&self, f: impl FnOnce()) {
        f();
    }

    /// Record an attribute on the current span.
    ///
    /// # Example
    ///
    /// ```rust
    /// let guard = ScopeGuard::new(lifecycle_span.clone());
    /// guard.record("needle.bead.outcome", "success");
    /// ```
    pub fn record(&self, key: &str, value: impl tracing::Value) {
        tracing::Span::current().record(key, value);
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        // Explicit cleanup - dropping _guard exits the span
        self._guard.take();
    }
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
        assert_eq!(strand_results::SKIPPED, "skipped");

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

    #[test]
    fn scope_guard_lifo_unwinding() {
        let outer = tracing::info_span!("outer");
        let inner = tracing::info_span!("inner");

        {
            let _outer_guard = ScopeGuard::new(outer);
            {
                let _inner_guard = ScopeGuard::new(inner);
                // Inner span is active here
                // (We can't directly test this without accessing tracing internals,
                // but we verify the guard compiles and runs without panicking)
            }
            // Inner span dropped here (correct LIFO)
        }
        // Outer span dropped here
    }

    #[test]
    fn scope_guard_record_attribute() {
        let span = tracing::info_span!(
            "test",
            needle.bead.id = "test-bf-123",
            needle.bead.priority = 5
        );

        let guard = ScopeGuard::new(span);
        guard.record("needle.bead.outcome", "success");
        // Guard dropped here
    }

    #[test]
    fn scope_guard_enter_scope() {
        let span = tracing::info_span!("test");
        let guard = ScopeGuard::new(span.clone());

        guard.enter_scope(|| {
            // This code runs within the span
            span.record("test_attr", "value");
        });

        // Guard still active after enter_scope
        guard.record("after_enter_scope", "another_value");
    }

    #[test]
    fn scope_guard_with_early_return() {
        let span = tracing::info_span!("test");
        let _guard = ScopeGuard::new(span);

        // Early return should still unwind the guard
        if true {
            return;
        }

        // Unreachable code
        panic!("should not reach here");
    }

    /// Regression test for needle-6da00123.
    ///
    /// `ScopeGuard::new` used to enter a by-value `Span` and transmute the
    /// resulting `Entered<'_>` to `Entered<'static>`, so the returned guard
    /// carried a dangling `&Span` from the moment it was created. Every
    /// existing test here dropped the guard while the dead stack slot still
    /// held usable bytes, which is why they passed; dropping one after the
    /// slot had been reused dispatched `exit()` through freed stack and
    /// segfaulted (null vtable dispatch, fault at ip=0).
    ///
    /// This test reproduces that shape: the guards are created inside
    /// `create_orphaned_guards` from *temporaries*, so the bytes they point
    /// at belong to that function's stack frame, and the frame dies on
    /// return. `smash_stack` then reuses that exact region with garbage, and
    /// only afterwards are the guards dropped. A guard that still borrows a
    /// dead temporary faults here; the owning `EnteredSpan` guard cannot.
    ///
    /// The two helpers are `#[inline(never)]` so the optimized test profile
    /// (`opt-level = 2`) cannot collapse the frame boundaries this depends
    /// on.
    #[test]
    fn scope_guard_survives_the_death_of_the_span_it_entered() {
        const GUARDS: usize = 32;

        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            // The temporary `Span`s handed to `ScopeGuard::new` die with this
            // call's frame; the guards keep pointing into it.
            let guards = create_orphaned_guards(GUARDS);

            // Reuse the frame region the temporaries lived in.
            smash_stack(0);
            std::hint::black_box(&guards);

            // Drop in LIFO order, per the guard's contract.
            for guard in guards.into_iter().rev() {
                drop(guard);
            }
        });
    }

    /// Build guards from temporary spans, so the spans' storage belongs to
    /// this function's frame rather than to the caller's.
    #[inline(never)]
    fn create_orphaned_guards(count: usize) -> Vec<ScopeGuard> {
        (0..count)
            .map(|_| ScopeGuard::new(tracing::info_span!("scope_guard.orphaned_temporary")))
            .collect()
    }

    /// Recurse shallowly, writing a non-canonical pointer pattern over every
    /// frame, to overwrite whatever stack the temporaries handed to
    /// `ScopeGuard::new` occupied. Dropping a guard that still borrows one
    /// then dereferences garbage instead of a live `Span`.
    #[inline(never)]
    fn smash_stack(depth: usize) {
        const TRASH: u64 = 0xA5A5_A5A5_A5A5_A5A5; // non-canonical on x86-64
        const MAX_DEPTH: usize = 64;
        let frame = [TRASH; 128]; // 1 KiB of garbage per frame
        if depth < MAX_DEPTH {
            smash_stack(depth + 1);
        }
        std::hint::black_box(&frame);
    }
}
