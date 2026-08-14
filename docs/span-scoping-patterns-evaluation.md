# Span Scoping Patterns Evaluation

**Task:** Research and evaluate span scoping patterns for replacing EnteredSpan guards with LIFO-safe async scoping

**Context:** The bf-3uj6i incident demonstrated that `EnteredSpan` guards held across `.await` points cause massive span leaks when Tokio reschedules tasks across worker threads. This document evaluates three alternative patterns.

---

## The Problem: LIFO Safety in Async Contexts

### What Went Wrong (bf-3uj6i)

`Entered`/`EnteredSpan` guards mutate a **thread-local span stack** using LIFO (Last-In-First-Out) discipline. When async tasks resume on different threads after `.await`:

1. `guard.enter()` pushes span onto Thread A's stack
2. `.await` suspends task  
3. Tokio resumes task on Thread B
4. `guard.drop()` runs on Thread B, tries to pop from Thread B's stack
5. **The entry on Thread A's stack is orphaned permanently**

### Consequences

- **Span leaks**: One `bead.claim` + one `bead.lifecycle` leaked per worker cycle
- **Quadratic growth**: Tracing's `fmt` layer re-serializes the entire span stack on every event
- **Disk filling**: Measured growth from 18 deep/4,983-byte lines → 2,488 deep/629,829-byte lines  
- **Massive output**: Reached ~159 GB/hr, filling a 444 GB disk

### Core Requirements

1. **LIFO safety**: Must not violate thread-local span stack invariants
2. **Async compatibility**: Must work correctly across `.await` boundaries
3. **Telemetry preservation**: Must maintain ability to record structured fields
4. **Concurrent claim cycles**: Must support multiple concurrent worker operations

---

## Pattern 1: Span with `.instrument()`

### Description

Create `tracing::Span` objects and attach them to futures using `.instrument()`, **never holding entered guards**.

```rust
// Create the span
let claim_span = tracing::info_span!(
    "bead.claim",
    needle.bead.id = %bead_id.as_ref(),
    needle.claim.retry_number = tracing::field::Empty,
    needle.claim.result = tracing::field::Empty,
);

// Record fields BEFORE instrumenting
claim_span.record("needle.claim.retry_number", 1u32);

// Attach span to the future
let claim = self.claimer.claim_one(...)
    .instrument(claim_span.clone())
    .await?;

// Store spans for later use (NOT entered guards)
self.bead_lifecycle_span = Some(lifecycle_span);

// Instrument subsequent operations
self.do_build().instrument(lifecycle_span.clone()).await?;
self.do_execute().instrument(lifecycle_span.clone()).await?;
```

### Pros

- ✅ **LIFO safe**: No entered guards, no thread-local stack manipulation
- ✅ **Async compatible**: Spans stored as plain values, safe across `.await`
- ✅ **Telemetry preserved**: All fields recordable via `.record()` before instrumentation
- ✅ **Concurrent-safe**: Each future gets its own span attachment, no shared mutable state
- ✅ **Tested in production**: Current NEEDLE implementation, proven stable post-bf-3uj6i
- ✅ **Explicit lifecycle**: Span attachment clear from code structure
- ✅ **Composable**: Can instrument nested futures with different spans

### Cons

- ❌ **Span per-future overhead**: Each instrumented call clones the span
- ❌ **Manual propagation**: Must remember to `.instrument()` every call
- ❌ **No automatic context**: Unlike guards, doesn't automatically apply to all code in scope
- ❌ **Verbose**: Every async operation needs explicit instrumentation

### LIFO Safety Analysis

**SAFE** - No entered guards means no thread-local stack manipulation. Spans are plain data types that can be freely cloned and passed across thread boundaries.

### Async Compatibility

**FULL** - Spans are `Clone + Send + 'static`, safe to store and use across await points.

### Telemetry Preservation

**FULL** - All fields recordable via `.record()`. The `tracing` crate's `instrument()` internally enters the span for the duration of the future, so all events within that future see the span as their parent.

### Concurrent Claim Cycle Support

**EXCELLENT** - Each claim/execution cycle creates its own span instances. No shared mutable state between concurrent operations.

---

## Pattern 2: `in_scope()` Pattern

### Description

Use `tracing::Span::in_scope()` to temporarily enter a span for a synchronous block, ensuring the span is active only for that block's duration.

```rust
let lifecycle_span = tracing::info_span!("bead.lifecycle", ...);

// For sync code within async fn
let result = lifecycle_span.in_scope(|| {
    // This sync code runs with the span entered
    tracing::info!("processing bead");
    do_sync_work()
});

// For async work, combine with instrument
let future_result = async move {
    lifecycle_span.in_scope(|| {
        // Sync setup
    });
    do_async_work().await
}.instrument(lifecycle_span.clone()).await;
```

### Pros

- ✅ **LIFO safe**: Span entered and exited within same synchronous block
- ✅ **Explicit scoping**: Clear boundaries where span is active
- ✅ **Low overhead**: No cloning for sync blocks
- ✅ **Telemetry preserved**: All fields recordable

### Cons

- ❌ **Limited to sync blocks**: Cannot hold scope across `.await` boundaries
- ❌ **Nesting complexity**: Requires careful ordering of `in_scope()` and `.await`
- ❌ **Mixed async/sync awkward**: Pattern breaks natural async flow
- ❌ **Error-prone**: Easy to accidentally wrap async work and break LIFO

### LIFO Safety Analysis

**CONDITIONALLY SAFE** - Safe *only* when used for purely synchronous blocks. If used to wrap async code that includes `.await`, LIFO violation occurs.

### Async Compatibility

**POOR** - Pattern is fundamentally synchronous. Cannot span `.await` boundaries without breaking LIFO. Would require splitting every async operation into pre/post sync blocks.

### Telemetry Preservation

**GOOD for sync, BROKEN for async** - Sync blocks preserve telemetry. Async work would lose span context at `.await` boundaries.

### Concurrent Claim Cycle Support

**GOOD** - Each operation has its own span, no shared state. However, the pattern's async limitations make it impractical for NEEDLE's state machine.

---

## Pattern 3: Thread-Local Span Stack with Async-Aware Guards (Experimental)

### Description

A custom span guard that detects thread migration and safely handles span stack restoration. This is a theoretical pattern not present in the current `tracing` crate but could be implemented.

```rust
struct AsyncSpanGuard {
    span: tracing::Span,
    original_thread_id: ThreadId,
}

impl AsyncSpanGuard {
    fn new(span: tracing::Span) -> Self {
        let thread_id = std::thread::current().id();
        span.enter();
        Self { span, original_thread_id: thread_id }
    }
}

impl Drop for AsyncSpanGuard {
    fn drop(&mut self) {
        let current_thread = std::thread::current().id();
        if current_thread == self.original_thread_id {
            // Safe to exit normally
            self.span.exit();
        } else {
            // Thread migration occurred - span already orphaned
            // Would need runtime to track and clean up orphaned spans
            // This is the hard part: no API to remove from specific thread's stack
        }
    }
}
```

### Pros

- ✅ **Familiar API**: RAII guard pattern matches user expectations
- ✅ **Automatic context**: All code in scope sees the span
- ✅ **Less verbose**: No manual instrumentation at each call site

### Cons

- ❌ **Not currently feasible**: `tracing` crate provides no API to manipulate other threads' span stacks
- ❌ **Requires runtime changes**: Would need tracing-span-level coordination or custom subscriber
- ❌ **Complex implementation**: Tracking orphaned spans across threads is non-trivial
- ❌ **Performance overhead**: Thread ID checks on every drop, potential contention
- ❌ **Unproven**: No production examples of this pattern in the wild

### LIFO Safety Analysis

**THEORETICAL** - Would require deep runtime support. The `tracing` crate's subscriber model assumes per-thread span stacks with no cross-thread manipulation APIs. Implementing this would likely require a custom subscriber with inter-thread coordination.

### Async Compatibility

**POTENTIALLY FULL** - If implemented correctly, guards could be held across `.await`. But the implementation complexity is extremely high.

### Telemetry Preservation

**FULL** - Same as normal guards when on the same thread. Cross-thread behavior depends on implementation.

### Concurrent Claim Cycle Support

**UNKNOWN** - Depends on implementation. Could have performance implications from thread coordination.

---

## Pattern Recommendation

### **Winner: Pattern 1 (Span with `.instrument()`)**

**Justification:**

1. **Proven in production**: Current NEEDLE implementation uses this pattern successfully post-bf-3uj6i
2. **Zero LIFO violations**: No entered guards means no thread-local stack manipulation
3. **Full async compatibility**: Spans are plain data, safe across thread boundaries
4. **Good concurrent support**: Each operation has independent span instances
5. **Maintainable**: Clear, explicit pattern that's easy to audit

**Pattern 2 (`in_scope()`)** is safe only for synchronous code and breaks NEEDLE's async state machine flow.

**Pattern 3 (Async-Aware Guards)** is theoretically interesting but not practically feasible without major changes to the `tracing` crate's subscriber model.

---

## Implementation Guidance for NEEDLE

### Current Best Practices (Already in Use)

```rust
// ✅ CORRECT: Create span, instrument future
let dispatch_span = tracing::info_span!(
    "agent.dispatch",
    needle.bead.id = %bead_id.as_ref(),
    needle.agent.pid = tracing::field::Empty,
);
let result = self.run_process(...).instrument(dispatch_span).await?;

// ✅ CORRECT: Store span for later instrumentation
self.bead_lifecycle_span = Some(lifecycle_span);

// ✅ CORRECT: Instrument each state transition
self.do_build().instrument(lifecycle_span.clone()).await?;
self.do_execute().instrument(lifecycle_span.clone()).await?;

// ❌ WRONG: Never do this
// let _guard = lifecycle_span.enter();  // Violates LIFO across await
// self.do_execute().await?;            // Task may resume on different thread
```

### Struct Field Documentation

All span fields MUST be documented:

```rust
/// The current bead lifecycle span. Created when a bead is claimed and
/// instrumented onto each state-handler future until the lifecycle ends.
///
/// **This must remain a `Span`, not an `EnteredSpan`**: entered guards mutate a
/// thread-local stack and cannot safely be stored across `.await` points.
bead_lifecycle_span: Option<tracing::Span>,
```

### Defensive Measures

- **Line length limits**: `log_writer.rs` caps line length at 10K chars to bound damage from span leaks
- **Explicit comments**: Multiple code comments reference bf-3uj6i to prevent regression
- **Testing**: Run workers under load to verify no span stack growth occurs

---

## Conclusion

Pattern 1 (Span with `.instrument()`) is the only pattern that simultaneously guarantees LIFO safety, full async compatibility, and proper telemetry preservation for NEEDLE's concurrent claim cycle workload. The pattern is already implemented and proven in production. No alternative pattern offers a better trade-off for this use case.

**Recommendation**: Continue using Pattern 1. Document the pattern in AGENTS.md or CLAUDE.md to prevent future regressions.
