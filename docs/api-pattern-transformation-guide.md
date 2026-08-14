# API Pattern Transformation Guide
## From EnteredSpan Guards to Span Instrumentation

**Purpose:** Document the transformation from unsafe `EnteredSpan` guards to safe `.instrument()` pattern with concrete before/after examples.

**Context:** The `bf-3uj6i` incident demonstrated that `EnteredSpan` guards held across `.await` points cause massive span leaks when Tokio reschedules tasks across worker threads (~159 GB/hr output). This guide shows the complete transformation.

---

## The Problem: Why Transformation Was Necessary

### EnteredSpan Guard Failure Mode

```rust
// ❌ DANGEROUS: Before pattern (bf-3uj6i bug)
async fn process_bead(bead_id: BeadId) -> Result<()> {
    let lifecycle_span = tracing::info_span!("bead.lifecycle");
    let _guard = lifecycle_span.enter();  // Pushes to Thread A's stack
    
    // Some async work
    do_phase_one().await?;  // Task may resume on Thread B
    
    do_phase_two().await?;  // _guard.drop() runs on Thread B, can't pop Thread A's entry
    
    Ok(())
}
// Result: Thread A's span stack grows by 1 per cycle → 2,488 deep → disk fills
```

### Why It Failed

1. `guard.enter()` pushes span onto **Thread A's** thread-local span stack
2. `.await` suspends task, Tokio may resume on **Thread B**
3. `guard.drop()` runs on **Thread B**, tries to pop from **Thread B's** stack
4. **The entry on Thread A's stack is orphaned permanently**
5. Tracing's `fmt` layer re-serializes entire stack on every event → quadratic growth

---

## Transformation Overview

### Before: EnteredSpan Guards (Unsafe)

```rust
// ❌ OLD PATTERN (DO NOT USE)
let span = tracing::info_span!("operation");
let _guard = span.enter();  // RAII guard manipulates thread-local stack
do_async_work().await?;     // UNSAFE across await
```

### After: Span Instrumentation (Safe)

```rust
// ✅ NEW PATTERN (CORRECT)
let span = tracing::info_span!("operation");
do_async_work().instrument(span).await?;  // Span attached to future
```

---

## Complete API Surface

### Core Types

```rust
use tracing::Span;           // Span object (Clone + Send + 'static)
use tracing::Instrument;     // .instrument() method for futures
```

### Key Methods

| Method | Purpose | Thread Safety |
|--------|---------|---------------|
| `tracing::info_span!()` | Create span with fields | ✅ Safe (creates plain data) |
| `Span::record()` | Record deferred fields | ✅ Safe (modifies span data) |
| `Future::instrument()` | Attach span to future | ✅ Safe (re-enters on poll) |
| `Span::in_scope()` | Execute sync closure | ✅ Safe (for sync-only code) |
| `Span::enter()` ❌ | Create entered guard | ❌ Unsafe across await |

### Helper Functions (src/span/mod.rs)

```rust
// Record structured fields on spans
crate::span::record_outcome(&span, "success");
crate::span::record_span_error(&span, "operation failed");
crate::span::record_strand_result(&span, "bead_found");
crate::span::record_claim_result(&span, "succeeded");
```

---

## Usage Pattern Catalog

### Pattern 1: Single Claim (Basic)

**Before:**
```rust
// ❌ OLD: EnteredSpan guard across await
async fn claim_bead(&self, bead_id: &BeadId) -> Result<ClaimResult> {
    let span = tracing::info_span!("bead.claim",
        needle.bead.id = %bead_id.as_ref()
    );
    let _guard = span.enter();  // DANGEROUS
    
    self.claim_one(bead_id).await?  // May resume on different thread
    // _guard drops here, can't clean up original thread's stack entry
}
```

**After:**
```rust
// ✅ NEW: Instrument future with span
async fn claim_bead(&self, bead_id: &BeadId) -> Result<ClaimResult> {
    let span = tracing::info_span!("bead.claim",
        needle.bead.id = %bead_id.as_ref(),
        needle.claim.retry_number = tracing::field::Empty,
        needle.claim.result = tracing::field::Empty,
    );
    
    // Record retry number BEFORE instrumentation
    span.record("needle.claim.retry_number", 1u32);
    
    // Attach span to future (re-enters on each poll)
    self.claim_one(bead_id)
        .instrument(span.clone())
        .await?
}
```

---

### Pattern 2: Sequential State Transitions

**Before:**
```rust
// ❌ OLD: Long-lived guard across multiple awaits
async fn process_bead(&mut self, bead: Bead) -> Result<()> {
    let lifecycle_span = tracing::info_span!("bead.lifecycle",
        needle.bead.id = %bead.id
    );
    let _guard = lifecycle_span.enter();  // Lives ENTIRE lifecycle
    
    self.build_prompt(&bead).await?;
    self.dispatch_agent(&bead).await?;
    self.handle_outcome(&bead).await?;
    // _guard drops here, leaked multiple stack entries
}
```

**After:**
```rust
// ✅ NEW: Store span, instrument each phase
struct Worker {
    bead_lifecycle_span: Option<tracing::Span>,
}

async fn process_bead(&mut self, bead: Bead) -> Result<()> {
    let lifecycle_span = tracing::info_span!("bead.lifecycle",
        needle.bead.id = %bead.id,
        needle.bead.priority = bead.priority,
        needle.bead.outcome = tracing::field::Empty,
    );
    
    // Store span for later use (NOT an entered guard)
    self.bead_lifecycle_span = Some(lifecycle_span.clone());
    
    // Instrument each phase with the same span
    self.build_prompt(&bead)
        .instrument(lifecycle_span.clone())
        .await?;
    
    self.dispatch_agent(&bead)
        .instrument(lifecycle_span.clone())
        .await?;
    
    self.handle_outcome(&bead)
        .instrument(lifecycle_span.clone())
        .await?;
    
    // Record final outcome
    lifecycle_span.record("needle.bead.outcome", "success");
    
    Ok(())
}
```

---

### Pattern 3: Concurrent Claims (Multiple Workers)

**Before:**
```rust
// ❌ OLD: Shared guard across concurrent tasks
async fn process_concurrent(beads: Vec<Bead>) -> Vec<Result<()>> {
    let span = tracing::info_span!("concurrent_processing");
    let _guard = span.enter();  // GUARD SHARED ACROSS TASKS
    
    let tasks: Vec<_> = beads.into_iter().map(|bead| {
        tokio::spawn(async move {
            process_single_bead(bead).await  // Each task inherits the guard context
        })
    }).collect();
    
    futures::future::join_all(tasks).await
    // Multiple tasks dropping guards on different threads → massive leaks
}
```

**After:**
```rust
// ✅ NEW: Each task gets its own span
async fn process_concurrent(beads: Vec<Bead>) -> Vec<Result<()>> {
    let tasks: Vec<_> = beads.into_iter().map(|bead| {
        let bead_id = bead.id.clone();
        let span = tracing::info_span!("bead.processing",
            needle.bead.id = %bead_id
        );
        
        tokio::spawn(
            process_single_bead(bead)
                .instrument(span)  // Each task independent
        )
    }).collect();
    
    futures::future::join_all(tasks).await
}
```

---

### Pattern 4: Nested Spans (Parent-Child)

**Before:**
```rust
// ❌ OLD: Nested guards across await
async fn process_with_metrics(&self, bead: &Bead) -> Result<()> {
    let parent = tracing::info_span!("bead.processing");
    let _parent_guard = parent.enter();
    
    let child = tracing::info_span!("bead.dispatch");
    let _child_guard = child.enter();  // Nested on same thread
    
    self.dispatch(bead).await?;  // Both guards unsafe across await
    // _child_guard drops, then _parent_guard drops — both may leak
}
```

**After:**
```rust
// ✅ NEW: Instrument nested futures
async fn process_with_metrics(&self, bead: &Bead) -> Result<()> {
    let parent = tracing::info_span!("bead.processing",
        needle.bead.id = %bead.id
    );
    
    let child = tracing::info_span!("bead.dispatch",
        needle.agent.pid = tracing::field::Empty
    );
    
    // Child span nested inside parent via instrumentation
    async move {
        self.dispatch(bead).await?
    }.instrument(child).instrument(parent).await
}
```

---

### Pattern 5: Deferred Field Recording

**Before:**
```rust
// ❌ OLD: Field recording with guard
async fn record_agent_result(&self, bead_id: &BeadId) -> Result<()> {
    let span = tracing::info_span!("agent.execution",
        needle.agent.exit_code = tracing::field::Empty
    );
    let _guard = span.enter();
    
    let result = self.run_agent().await?;
    
    // Recording while guard held (unsafe context)
    span.record("needle.agent.exit_code", result.exit_code);
    
    Ok(())
}
```

**After:**
```rust
// ✅ NEW: Field recording within instrumented future
async fn record_agent_result(&self, bead_id: &BeadId) -> Result<()> {
    let span = tracing::info_span!("agent.execution",
        needle.agent.exit_code = tracing::field::Empty
    );
    
    async move {
        let result = self.run_agent().await?;
        
        // Record using Span::current() within instrumented future
        tracing::Span::current().record("needle.agent.exit_code", result.exit_code);
        
        Ok::<(), Error>(())
    }.instrument(span).await?
}
```

---

### Pattern 6: Sync Code Within Async (in_scope)

**Before:**
```rust
// ❌ OLD: Guard for sync work in async context
async fn mixed_sync_async(&self) -> Result<()> {
    let span = tracing::info_span!("processing");
    let _guard = span.enter();
    
    // Sync work
    let config = self.load_config();  // OK (no await)
    
    // Async work
    self.process(&config).await?;  // UNSAFE (guard across await)
    
    Ok(())
}
```

**After:**
```rust
// ✅ NEW: in_scope for sync, instrument for async
async fn mixed_sync_async(&self) -> Result<()> {
    let span = tracing::info_span!("processing",
        needle.config = tracing::field::Empty
    );
    
    // Record sync field using in_scope
    let config = span.in_scope(|| {
        let cfg = self.load_config();
        span.record("needle.config", &cfg.name);
        cfg
    });
    
    // Instrument async work
    self.process(&config).instrument(span).await?;
    
    Ok(())
}
```

---

### Pattern 7: Strand Waterfall Evaluation

**Before:**
```rust
// ❌ OLD: Guard across sequential strand evaluation
async fn run_waterfall(&self) -> Result<Option<Bead>> {
    let waterfall_span = tracing::info_span!("strand.waterfall");
    let _guard = waterfall_span.enter();
    
    let pluck_result = self.evaluate_pluck().await?;
    if pluck_result.is_some() { return Ok(pluck_result); }
    
    let mend_result = self.evaluate_mend().await?;
    if mend_result.is_some() { return Ok(mend_result); }
    
    let explore_result = self.evaluate_explore().await?;
    Ok(explore_result)
}
```

**After:**
```rust
// ✅ NEW: Each strand independently instrumented
async fn run_waterfall(&self) -> Result<Option<Bead>> {
    'waterfall: loop {
        for strand in &self.strands {
            let strand_span = tracing::info_span!(
                "strand.{}",
                strand.name(),
                needle.strand.name = %strand.name(),
                needle.strand.result = tracing::field::Empty,
            );
            
            let start = Instant::now();
            let result = strand.evaluate(...)
                .instrument(strand_span.clone())
                .await?;
            
            let elapsed_ms = start.elapsed().as_millis() as u64;
            
            // Record result using in_scope (sync only)
            strand_span.in_scope(|| {
                tracing::Span::current().record("needle.strand.result", &result_str);
                tracing::Span::current().record("needle.strand.duration_ms", elapsed_ms);
            });
            
            match result {
                StrandResult::BeadFound(beads) => {
                    if let Some(bead) = beads.into_iter().next() {
                        return Ok(Some(bead));
                    }
                }
                StrandResult::WorkCreated => {
                    continue 'waterfall;  // Restart from Pluck
                }
                StrandResult::NoWork => continue,
                _ => continue,
            }
        }
        return Ok(None);  // All strands exhausted
    }
}
```

---

### Pattern 8: Runtime Guard (CLI Bootstrap)

**Before:**
```rust
// ❌ OLD: Runtime guard before tokio::spawn
fn main() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let _rt_guard = rt.enter();  // Guard for thread-local runtime
    
    // This spawn is protected by guard
    tokio::spawn(async {
        // Work here
    });
    
    rt.block_on(async {
        worker.run().await?
    })
}
```

**After:**
```rust
// ✅ NEW: Runtime guard acceptable (no async in setup), then block_on
fn main() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let _rt_guard = rt.enter();  // OK for sync setup only
    
    // All spawns during setup protected by guard
    let telemetry = init_telemetry();
    
    // Enter runtime block - guard no longer needed
    rt.block_on(async {
        // All async work here instrumented normally
        worker.run().instrument(session_span).await?
    })
}
```

---

## Field Recording Patterns

### Pattern A: Static Fields (Known at Creation)

```rust
// ✅ Static fields in macro
let span = tracing::info_span!(
    "bead.lifecycle",
    needle.bead.id = %bead_id.as_ref(),      // Display impl
    needle.bead.priority = bead.priority,     // Direct value
    needle.bead.title_hash = %title_hash,     // Reference
);
```

### Pattern B: Deferred Fields (Unknown at Creation)

```rust
// ✅ Deferred fields with Empty + record()
let span = tracing::info_span!(
    "agent.execution",
    needle.agent.exit_code = tracing::field::Empty,  // Placeholder
);

// Later: record before or during instrumentation
span.record("needle.agent.exit_code", exit_code);

// Or within instrumented future
tracing::Span::current().record("needle.agent.exit_code", exit_code);
```

### Pattern C: Helper Functions

```rust
// ✅ Use helpers for common patterns
use crate::span::{record_outcome, record_span_error};

lifecycle_span.in_scope(|| {
    match outcome {
        Outcome::Success => record_outcome(&lifecycle_span, "success"),
        Outcome::Failure => {
            record_outcome(&lifecycle_span, "failure");
            record_span_error(&lifecycle_span, "agent failed");
        }
    }
});
```

---

## Critical Differences Summary

| Aspect | EnteredSpan Guards | Instrumentation Pattern |
|--------|-------------------|------------------------|
| **Thread safety** | ❌ Unsafe across await | ✅ Safe (plain data) |
| **LIFO guarantee** | ❌ Violated at await | ✅ Never violated |
| **Span stack** | Mutated per-thread | Not mutated |
| **Async behavior** | ❌ Leaks on thread migration | ✅ Re-enters on poll |
| **Concurrent use** | ❌ Guards conflict | ✅ Independent spans |
| **Memory overhead** | ❌ Stack grows unbounded | ✅ Bounded (refcount) |
| **Field recording** | ✅ Available | ✅ Available |
| **Nesting support** | ❌ Unsafe | ✅ Safe (instrument nesting) |
| **Error handling** | ❌ Guards may not drop | ✅ Futures always complete |

---

## Common Mistakes to Avoid

### ❌ Mistake 1: Holding guards across await

```rust
// WRONG: Guard lives across await
let _guard = span.enter();
some_async_work().await?;
```

### ✅ Correction 1: Instrument the future

```rust
// CORRECT
some_async_work().instrument(span).await?;
```

### ❌ Mistake 2: Storing entered guards

```rust
// WRONG: Storing EnteredSpan in struct
struct Worker {
    lifecycle_guard: tracing::Entered<'span>,  // ❌ Never do this
}
```

### ✅ Correction 2: Store spans, not guards

```rust
// CORRECT: Store plain Span
struct Worker {
    bead_lifecycle_span: Option<tracing::Span>,  // ✅ Safe
}
```

### ❌ Mistake 3: Nested guards in async

```rust
// WRONG: Multiple guards across await
let _parent = parent_span.enter();
let _child = child_span.enter();
async_work().await?;
```

### ✅ Correction 3: Instrument nested futures

```rust
// CORRECT
async_work()
    .instrument(child_span)
    .instrument(parent_span)
    .await?;
```

### ❌ Mistake 4: Using in_scope for async code

```rust
// WRONG: in_scope wraps async work
span.in_scope(|| {
    async_work().await  // ❌ in_scope is for sync only!
});
```

### ✅ Correction 4: Use instrument for async

```rust
// CORRECT
async_work().instrument(span).await?;
```

---

## Verification Checklist

Before committing code, verify:

- [ ] No `let _guard = span.enter()` held across `.await`
- [ ] All `EnteredSpan` or `Entered` types eliminated
- [ ] Spans stored in structs are `tracing::Span`, not guards
- [ ] Async operations use `.instrument(span)`
- [ ] Sync-only code uses `span.in_scope(|| ...)` correctly
- [ ] Deferred fields use `tracing::field::Empty` + `.record()`
- [ ] Concurrent operations have independent span instances
- [ ] Nested spans use chained `.instrument()` calls

---

## Performance Characteristics

### Memory Overhead

| Pattern | Per-Span Overhead | Growth Characteristics |
|---------|------------------|----------------------|
| EnteredSpan | ~24 bytes (guard) | ❌ Linear with leaked guards |
| Instrumentation | ~16 bytes (Arc<Span>) | ✅ Bounded by active futures |

### CPU Overhead

- `.instrument()`: Single Arc::clone() per future (atomic increment)
- `.enter()`: Thread-local stack push/pop (unsafe across await)
- `Span::record()`: Atomic store (same for both patterns)

### Measured Impact (bf-3uj6i)

**Before (EnteredSpan):**
- Span stack depth: 18 → 2,488 (138x growth)
- Line length: 4,983 → 629,829 bytes (126x growth)
- Output rate: ~159 GB/hr
- Disk filled: 444 GB in ~3 hours

**After (Instrumentation):**
- Span stack depth: Constant ~3-5 active spans
- Line length: Bounded <10K chars (log_writer.rs cap)
- Output rate: ~10 MB/hr
- Disk usage: Stable

---

## Testing Guidance

### Unit Tests

```rust
#[tokio::test]
async fn span_instrumentation_test() {
    let span = tracing::info_span!("test");
    
    // Verify span is current within instrumented future
    let result = async {
        assert!(tracing::Span::current().is_none());
        "ok".to_string()
    }.instrument(span.clone()).await;
    
    assert_eq!(result, "ok");
}
```

### Load Tests

Run multiple workers under concurrent load to verify:

1. **No span stack growth**: Monitor log line lengths stay bounded
2. **No memory increase**: Heap profile stable over time
3. **Field preservation**: All 35 telemetry fields present in output

### Regression Test

```rust
// Verify no EnteredSpan types in codebase
// Run: grep -r "EnteredSpan\|Entered<" src/
// Expected: No matches (only comments/docs)
```

---

## Further Reading

- **Span Scoping Evaluation**: `/docs/span-scoping-patterns-evaluation.md` — Complete analysis of 3 patterns
- **Field Capture Strategy**: `/docs/telemetry-field-capture-strategy.md` — All 35 telemetry fields documented
- **Original Incident**: Bead `bf-3uj6i` — Full postmortem of span leak disaster
- **Span Helpers**: `/src/span/mod.rs` — Helper functions for field recording
- **Implementation**: `/src/worker/mod.rs` — Production implementation of pattern

---

## Quick Reference Card

```rust
// ✅ CORRECT PATTERNS (use these)

// Basic async operation
let span = tracing::info_span!("operation", id = %id);
async_work().instrument(span).await?;

// Store span for reuse
self.my_span = Some(span.clone());

// Record deferred field
span.record("field.name", value);
tracing::Span::current().record("field.name", value);

// Sync-only code
span.in_scope(|| { sync_work() });

// Nested spans
async_work()
    .instrument(child_span)
    .instrument(parent_span)
    .await?;

// ❌ WRONG PATTERNS (never do these)

// Guard across await
let _guard = span.enter();
async_work().await?;

// Store entered guard
struct Bad { guard: tracing::Entered }

// in_scope for async
span.in_scope(|| async_work().await);

// Multiple guards
let _g1 = s1.enter();
let _g2 = s2.enter();
async_work().await?;
```

---

**Transformation Status:** ✅ **COMPLETE** — NEEDLE codebase fully migrated to instrumentation pattern. No EnteredSpan guards remain in production code.

**Last Updated:** 2026-08-14 (bf-4sc6q documentation task)
