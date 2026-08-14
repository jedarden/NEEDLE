# Span-Scope Preservation Pattern Design

## Problem Statement

The current NEEDLE worker uses `tracing::Span` for telemetry hierarchy with `in_scope()` for synchronous code sections. This creates LIFO (Last-In-First-Out) violations when multiple claim cycles run concurrently across different execution contexts, causing scope leakage where telemetry from one bead's lifecycle bleeds into another's.

**Root Cause:** `EnteredSpan` guards mutate a thread-local stack and cannot safely be stored across `.await` points. The current workaround stores `Span` directly and uses `in_scope()`, but this is not LIFO-safe in async contexts with concurrent claim cycles.

## Current State Analysis

### Existing Pattern
```rust
// Worker stores lifecycle span as Span (not EnteredSpan)
bead_lifecycle_span: Option<tracing::Span>

// Usage in state machine:
lifecycle_span.in_scope(|| self.do_log())?
do_execute().instrument(lifecycle_span.clone()).await
```

### Telemetry Attributes to Preserve
```rust
needle.bead.id = %bead_id
needle.bead.priority = %priority
needle.bead.title_hash = %title_hash
needle.bead.outcome = %outcome  // Set on completion
```

### Current Span Hierarchy
```
worker.session (root)
└── strand.pluck
    └── bead.lifecycle              ← Stored across .await
        ├── bead.claim             (ATOMIC phase)
        ├── bead.prompt_build
        ├── agent.dispatch
        │   └── agent.execution
        └── bead.outcome
            └── bead.mitosis?
```

## Design Decision: ScopeGuard Pattern with Explicit Instrumentation

**Chosen Approach:** Replace `in_scope()` with a `ScopeGuard` that uses explicit `.instrument()` on futures and a RAII guard for synchronous sections, ensuring LIFO-compliant scope management.

### Why This Pattern

1. **LIFO Compliance:** ScopeGuard explicitly unwinds in reverse order on drop
2. **Async Safety:** No thread-local mutations across `.await` points
3. **Attribute Preservation:** Span attributes are captured at creation time
4. **Zero Overhead:** Compile-time guard with runtime cost equal to current approach
5. **Ergonomics:** Drop-in replacement for `in_scope()` with minimal code changes

### Rejected Alternatives

1. **Span with in_scope():** Current approach - not LIFO-safe, causes scope leakage
2. **EnteredSpan storage:** Violates thread-local safety across `.await`
3. **Manual span enter/exit:** Error-prone, easy to forget cleanup
4. **Async scope guards:** Requires runtime support, adds overhead

## Proposed API

### Core Type: ScopeGuard

```rust
/// RAII guard for LIFO-safe span scoping.
///
/// When dropped, the span is explicitly closed, ensuring proper unwinding
/// even across early returns or errors.
pub struct ScopeGuard {
    span: Span,
    _guard: Option<tracing::span::EnteredSpan>,
}

impl ScopeGuard {
    /// Create a new scope guard from a span, entering it immediately.
    ///
    /// The span will be exited when the guard is dropped (LIFO order).
    pub fn new(span: Span) -> Self {
        let _guard = Some(span.enter());
        Self { span, _guard }
    }

    /// Enter the span scope for a synchronous block.
    ///
    /// Prefer `ScopeGuard::new()` over this method for automatic cleanup.
    /// This method exists for cases where manual scope control is needed.
    pub fn enter_scope(&self, f: impl FnOnce()) {
        let _guard = self.span.enter();
        f();
    }

    /// Record an attribute on the span.
    pub fn record(&self, key: &str, value: impl tracing::Value) {
        self.span.record(key, value);
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        // Explicit cleanup - _guard drop handles the actual exit
        self._guard.take();
    }
}
```

### Usage Examples

#### Before (Current Pattern - LIFO-unsafe)
```rust
lifecycle_span.in_scope(|| self.do_log())?
```

#### After (ScopeGuard Pattern - LIFO-safe)
```rust
// Option 1: Direct guard (preferred)
let _guard = ScopeGuard::new(lifecycle_span.clone());
self.do_log()?;

// Option 2: Explicit scope
lifecycle_guard.enter_scope(|| self.do_log())?;
```

#### Async Context (No Change Needed)
```rust
// Already safe - instrument() doesn't mutate thread-local
do_execute().instrument(lifecycle_span.clone()).await
```

## Implementation Strategy

### Phase 1: Core Infrastructure
1. Add `ScopeGuard` type to `src/span/mod.rs`
2. Add unit tests for LIFO compliance
3. Add integration test with concurrent claim cycles

### Phase 2: Worker Migration
1. Replace `lifecycle_span.in_scope()` calls in `worker/mod.rs`
2. Verify no `EnteredSpan` is stored across `.await`
3. Add compile-time assertions for span type safety

### Phase 3: Validation
1. Run integration tests with concurrent workers
2. Verify telemetry attributes preserved across all span types
3. Check for scope leakage in logs

## Telemetry Field Preservation

### Span Creation (Current - Preserved)
```rust
let lifecycle_span = tracing::info_span!(
    "bead.lifecycle",
    needle.bead.id = %bead_id,
    needle.bead.priority = %priority,
    needle.bead.title_hash = %title_hash,
    needle.bead.outcome = tracing::field::Empty,  // Set later
);
```

### Attribute Updates (New API)
```rust
// Instead of: lifecycle_span.record("needle.bead.outcome", outcome)
lifecycle_guard.record("needle.bead.outcome", outcome);
```

### Verification Checklist
- [ ] `needle.bead.id` captured at span creation
- [ ] `needle.bead.priority` captured at span creation
- [ ] `needle.bead.title_hash` captured at span creation
- [ ] `needle.bead.outcome` set on completion (via ScopeGuard::record)
- [ ] All fields present in exported OTLP traces
- [ ] No field loss during scope transitions

## Scope Unwinding Strategy

### LIFO Compliance

The `ScopeGuard` ensures LIFO unwinding through RAII:

```rust
// Multiple scopes nest correctly (LIFO)
let _outer = ScopeGuard::new(outer_span);
{
    let _inner = ScopeGuard::new(inner_span);
    // do_work() - inner span active
} // inner span dropped here (correct LIFO)
// outer span still active
```

### Multiple Claim Cycles

When a worker completes a bead and claims another:

```rust
// Cycle 1: Claim and process bead A
let lifecycle_a = create_lifecycle_span(&bead_a);
let _guard_a = ScopeGuard::new(lifecycle_a);
process_bead_a().await;
// _guard_a dropped here - span A closed

// Cycle 2: Claim and process bead B
let lifecycle_b = create_lifecycle_span(&bead_b);
let _guard_b = ScopeGuard::new(lifecycle_b);
process_bead().await;
// No leakage from A to B
```

### Async Context Safety

```rust
// Safe: No guard across .await
let lifecycle_span = self.bead_lifecycle_span.clone().unwrap();
do_execute().instrument(lifecycle_span).await?;

// Unsafe: Guard stored across .await (CATCH THIS IN CODE REVIEW)
let guard = ScopeGuard::new(span); // ❌ NEVER DO THIS
long_async_operation().await; // UB - guard dropped after await
```

## Before/After Transformation

### Example 1: do_log() call

**Before:**
```rust
WorkerState::Logging => lifecycle_span.in_scope(|| self.do_log())?,
```

**After:**
```rust
WorkerState::Logging => {
    let _guard = ScopeGuard::new(lifecycle_span.clone());
    self.do_log()
}?,
```

### Example 2: Multiple nested scopes

**Before:**
```rust
outer_span.in_scope(|| {
    inner_span.in_scope(|| {
        do_work()
    })
})
```

**After:**
```rust
let _outer = ScopeGuard::new(outer_span.clone());
{
    let _inner = ScopeGuard::new(inner_span.clone());
    do_work()
} // inner closed
// outer still active
```

### Example 3: State machine with multiple handlers

**Before:**
```rust
WorkerState::Building => self.do_build().instrument(lifecycle_span.clone()).await?,
WorkerState::Dispatching => {
    self.do_dispatch().instrument(lifecycle_span.clone()).await?;
    self.do_execute().instrument(lifecycle_span.clone()).await?;
},
WorkerState::Logging => lifecycle_span.in_scope(|| self.do_log())?,
```

**After:**
```rust
WorkerState::Building => self.do_build().instrument(lifecycle_span.clone()).await?,
WorkerState::Dispatching => {
    self.do_dispatch().instrument(lifecycle_span.clone()).await?;
    self.do_execute().instrument(lifecycle_span.clone()).await?;
},
WorkerState::Logging => {
    let _guard = ScopeGuard::new(lifecycle_span.clone());
    self.do_log()
}?,
```

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scope_guard_lifo_unwinding() {
        let outer = tracing::info_span!("outer");
        let inner = tracing::info_span!("inner", parent = &outer);

        let _outer_guard = ScopeGuard::new(outer);
        {
            let _inner_guard = ScopeGuard::new(inner);
            // Inner span should be current
            assert!(span_is_current(&inner));
        }
        // Outer span should be current after inner dropped
        assert!(span_is_current(&outer));
    }

    #[test]
    fn test_attribute_preservation() {
        let span = tracing::info_span!(
            "test",
            needle.bead.id = "test-bf-123",
            needle.bead.priority = 5
        );

        let guard = ScopeGuard::new(span);
        guard.record("needle.bead.outcome", "success");

        // Verify attributes in span (implementation-specific)
    }
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_concurrent_claim_cycles() {
    // Spawn two workers claiming different beads
    // Verify no telemetry leakage between them
}

#[tokio::test]
async fn test_scope_unwinding_on_early_return() {
    let span = tracing::info_span!("test");
    let _guard = ScopeGuard::new(span);
    if true {
        return; // Guard should still unwind
    }
}
```

## Migration Plan

### Files to Modify
1. `src/span/mod.rs` - Add ScopeGuard type
2. `src/worker/mod.rs` - Replace in_scope() calls
3. `tests/integration_tests.rs` - Add scope tests

### Step-by-Step
1. **Add ScopeGuard type** to span module
2. **Add unit tests** for ScopeGuard behavior
3. **Find all in_scope() usage** via grep
4. **Replace each site** with ScopeGuard pattern
5. **Run full test suite** to verify no regressions
6. **Add integration test** for concurrent claim cycles
7. **Verify telemetry export** - check for field loss

### Rollback Strategy
If issues arise, the change is easily revertible:
- ScopeGuard is a new type (no existing API changes)
- Each in_scope() replacement is independent
- No data format changes (telemetry schema unchanged)

## Verification Checklist

### Code Quality
- [ ] No `EnteredSpan` stored across `.await` points
- [ ] All `in_scope()` calls replaced with `ScopeGuard`
- [ ] Async futures continue using `.instrument()`
- [ ] Compile-time warnings about unsafe scoping resolved

### Telemetry Integrity
- [ ] All span attributes present in exported traces
- [ ] No field loss during scope transitions
- [ ] Trace hierarchy preserved (worker.session → strand → bead.lifecycle)
- [ ] No telemetry leakage between concurrent claim cycles

### Testing
- [ ] Unit tests pass
- [ ] Integration tests pass
- [ ] Manual testing with concurrent workers
- [ ] Telemetry export validation

## Future Considerations

### Extensibility
The ScopeGuard pattern can be extended to support:
- **Span relationships:** Explicit parent-child tracking
- **Context propagation:** Thread-local context storage
- **Metrics collection:** Automatic metric recording on scope exit

### Performance
The design has zero runtime overhead compared to the current approach:
- `ScopeGuard::new()` = one allocation (the guard struct)
- Drop cost = one option `take()` call
- No additional synchronization or locking

### Alternative Futures
If Rust's tracing ecosystem evolves to support async-native scoping, the design can be adapted without API changes - `ScopeGuard` would become a thin wrapper around the new primitive.

## Conclusion

The ScopeGuard pattern provides LIFO-safe span scoping while preserving all existing telemetry attributes and maintaining the ergonomics of the current API. It addresses the root cause of scope leakage without requiring extensive refactoring or introducing runtime overhead.

The design is:
- **Safe:** No thread-local mutations across `.await`
- **Preserved:** All telemetry attributes maintained
- **Testable:** Clear unit and integration test strategy
- **Reversible:** Easy rollback if issues arise
- **Future-proof:** Extensible design for future enhancements

Implementation should proceed in phases, with validation at each step, to ensure the migration preserves both telemetry fidelity and system correctness.
