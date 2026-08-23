# Claim Path Span Usage Audit

**Date:** 2026-08-23  
**Scope:** Complete audit of all span operations in the NEEDLE claim path  
**Purpose:** Identify any EnteredSpan guards held across await points that could cause thread-local span stack corruption

## Executive Summary

**Result:** ✅ **ALL SAFE** - No span guards are held across await points in the claim path.

**Key Findings:**
- Total span operations catalogued: 31
- LIFO-safe operations: 31 (100%)
- Potential-await-risk: 0
- Needs-fix: 0

**Critical Pattern Correctly Applied:**
The claim path uses `.instrument()` to attach spans to futures, ensuring proper async context propagation without holding thread-local guards across await points.

---

## Span Hierarchy in Claim Path

```
strand.{name}                    (created in strand/mod.rs:281)
  └── bead.claim                 (created in worker/mod.rs:2101, instrumented at 2126)
       └── [store operations]    (claim/mod.rs uses Span::current() which resolves correctly)
```

---

## Detailed Catalog by Location

### 1. Span Creation (info_span! macros)

| Location | Span Name | Parent | Line | Safety |
|----------|-----------|--------|------|--------|
| `strand/mod.rs:281-285` | `strand.{name}` | (root) | LIFO-safe | Span creation only |
| `worker/mod.rs:2101-2106` | `bead.claim` | `strand.{name}` (via instrumentation) | LIFO-safe | Span creation only |
| `worker/mod.rs:2177-2183` | `bead.lifecycle` | (after claim succeeds) | LIFO-safe | Created post-claim |

**Code Examples:**

```rust
// strand/mod.rs:281-285
let strand_span = tracing::info_span!(
    "strand.{}",
    strand_name,
    needle.strand.name = %strand_name,
);
```

```rust
// worker/mod.rs:2101-2106
let claim_span = tracing::info_span!(
    "bead.claim",
    needle.bead.id = %bead_id.as_ref(),
    needle.claim.retry_number = tracing::field::Empty,
    needle.claim.result = tracing::field::Empty,
);
```

---

### 2. Span Instrumentation (.instrument())

| Location | Span | Target Future | Line | Safety |
|----------|------|---------------|------|--------|
| `strand/mod.rs:289` | `strand.{name}` | `strand.evaluate()` | LIFO-safe | Proper async instrumentation |
| `worker/mod.rs:2126` | `bead.claim` | `claimer.claim_one()` | **LIFO-safe** | **Critical: attaches span to async future** |
| `worker/mod.rs:1296` | `bead.lifecycle` | `do_build()` | LIFO-safe | Post-claim instrumentation |
| `worker/mod.rs:1299` | `bead.lifecycle` | `do_execute()` | LIFO-safe | Post-claim instrumentation |
| `worker/mod.rs:1303` | `bead.lifecycle` | `do_execute()` (alt) | LIFO-safe | Post-claim instrumentation |
| `worker/mod.rs:1306` | `bead.lifecycle` | `do_handle()` | LIFO-safe | Post-claim instrumentation |

**Critical Code - worker/mod.rs:2126:**

```rust
let claim = self
    .claimer
    .claim_one(&bead_id, &self.qualified_id(), &exclusions, Some(strand))
    .instrument(claim_span.clone())
    .await?;
```

**Why This Is Safe:**
- `.instrument()` attaches the span to the future itself
- When the future is polled on any thread, the span context is automatically entered/exited by the runtime
- No guard is held manually by application code
- This is the **correct pattern** for async code

**Comment from code (worker/mod.rs:2107-2119):**

```rust
// Do NOT hold an `Entered`/`EnteredSpan` guard here. Those guards mutate a
// thread-local span stack. The previous code kept one alive across the
// claim await; when Tokio resumed the task on a different worker thread,
// dropping the guard there could not remove the entry left on the original
// thread. The lifecycle guard had the same cross-await problem. One
// `bead.claim` plus one `bead.lifecycle` leaked per cycle. Because the fmt
// layer re-serializes the whole span stack on every event, output grew
// quadratically: measured at 18 deep / 4,983-byte lines early and 2,488 deep
// / 629,829-byte lines late, reaching ~159 GB/hr and filling a 444 GB disk.
// See bf-3uj6i.
//
// `.instrument()` attaches the span to the future correctly, so
// `Span::current()` inside `claim_one` (claim/mod.rs records
// needle.claim.result there) still resolves to this span.
```

---

### 3. Span Entry Guards (.enter())

| Location | Span | Timing | Line | Safety |
|----------|------|--------|------|--------|
| `strand/mod.rs:296` | `strand.{name}` | **AFTER await** | LIFO-safe | Synchronous bookkeeping only |

**Code - strand/mod.rs:289-296:**

```rust
let result = strand
    .evaluate(store, exclusions)
    .instrument(strand_span.clone())
    .await;  // <-- AWAIT HERE
let elapsed_ms = start.elapsed().as_millis() as u64;

// The remaining evaluation bookkeeping is synchronous, so a
// scoped guard is safe here and preserves strand context on its
// tracing events without crossing an `.await`.
let _strand_enter = strand_span.enter();  // <-- ENTER AFTER AWAIT (SAFE)
```

**Why This Is Safe:**
- The `.enter()` happens **AFTER** the `.await` on line 290
- The guard is only used for synchronous span attribute recording (lines 316-323)
- No await points occur while the guard is held
- Guard drops at end of loop iteration (line ~324)

**Comment from code (strand/mod.rs:293-295):**

```rust
// The remaining evaluation bookkeeping is synchronous, so a
// scoped guard is safe here and preserves strand context on its
// tracing events without crossing an `.await`.
```

---

### 4. Span::current().record() Operations

All operations in `claim/mod.rs` use `tracing::Span::current().record()` to write attributes to the currently active span. Because the `bead.claim` span is attached via `.instrument()` in `worker/mod.rs:2126`, `Span::current()` correctly resolves to that span throughout `claim_one()`.

| Location | Attribute | Value | Line | Safety |
|----------|-----------|-------|------|--------|
| `claim/mod.rs:183` | `needle.claim.result` | "max_retries_exceeded" | LIFO-safe | Resolves to instrumented span |
| `claim/mod.rs:185` | `otel.status_code` | 2u64 | LIFO-safe | Error status |
| `claim/mod.rs:186` | `otel.status_description` | "max_retries_exceeded" | LIFO-safe | Error description |
| `claim/mod.rs:194` | `needle.bead.id` | bead_id | LIFO-safe | Resolves to instrumented span |
| `claim/mod.rs:195` | `needle.claim.retry_number` | attempts | LIFO-safe | Retry tracking |
| `claim/mod.rs:215` | `otel.status_code` | 2u64 | LIFO-safe | Flock timeout error |
| `claim/mod.rs:216-217` | `otel.status_description` | flock timeout | LIFO-safe | Error context |
| `claim/mod.rs:233-235` | `otel.status_code/description` | verify failed | LIFO-safe | Store error |
| `claim/mod.rs:287` | `needle.claim.result` | "succeeded" | LIFO-safe | Success case |
| `claim/mod.rs:299` | `needle.claim.result` | "race_lost" | LIFO-safe | Race condition |
| `claim/mod.rs:312` | `needle.claim.result` | reason | LIFO-safe | Not claimable |
| `claim/mod.rs:320` | `needle.claim.result` | reason | LIFO-safe | Claim error |
| `claim/mod.rs:335-336` | `otel.status_code/description` | last_error | LIFO-safe | Error threshold |
| `claim/mod.rs:359-366` | `otel.status_code/description` | suspect info | LIFO-safe | Suspect bead |
| `claim/mod.rs:375` | `needle.claim.result` | reason | LIFO-safe | Store error |
| `claim/mod.rs:390-391` | `otel.status_code/description` | last_error | LIFO-safe | Error threshold |
| `claim/mod.rs:399-400` | `otel.status_code/description` | reason | LIFO-safe | Store error |
| `claim/mod.rs:407-410` | `needle.claim.result` / otel.status | "all_race_lost" | LIFO-safe | Exhausted retries |
| `claim/mod.rs:558-559` | `needle.bead.id` / `needle.claim.result` | bead.id / "succeeded" | LIFO-safe | Auto-claim success |
| `claim/mod.rs:569` | `needle.claim.result` | reason | LIFO-safe | Auto-claim failed |
| `claim/mod.rs:583-585` | `needle.claim.result` / otel.status | reason | LIFO-safe | Auto-claim error |

**Why These Are Safe:**
- All called within `claim_one()` which is instrumented with `bead.claim` span
- `Span::current()` resolves to the instrumented span due to tracing's async context propagation
- No manual guards involved - tracing runtime manages span entry/exit

---

## Safety Verification

### Pattern 1: Span Creation + .instrument() ✅ SAFE

**Example: worker/mod.rs:2101-2127**

```rust
let claim_span = tracing::info_span!("bead.claim", ...);

let claim = self
    .claimer
    .claim_one(...)
    .instrument(claim_span.clone())  // <-- CORRECT: Attach to future
    .await?;
```

**Safety:** ✅ Correct pattern for async code. No guard held by application.

---

### Pattern 2: .enter() AFTER await ✅ SAFE

**Example: strand/mod.rs:289-296**

```rust
let result = strand
    .evaluate(store, exclusions)
    .instrument(strand_span.clone())
    .await;  // <-- AWAIT COMPLETES HERE

// Now in synchronous code - safe to enter
let _strand_enter = strand_span.enter();
```

**Safety:** ✅ Guard only held during synchronous bookkeeping, no await points.

---

### Pattern 3: Span::current() in instrumented future ✅ SAFE

**Example: claim/mod.rs:183-410 (throughout file)**

```rust
// Inside claim_one(), which is instrumented with bead.claim span
tracing::Span::current().record("needle.claim.result", "succeeded");
```

**Safety:** ✅ `Span::current()` correctly resolves to instrumented span due to async context propagation.

---

## Historical Context: The Bug That Was Fixed

**Reference:** bead `bf-3uj6i` (mentioned in worker/mod.rs:2115)

**The Problem:**
Previous code held an `Entered` guard alive across the claim await:
```rust
// OLD BUGGY CODE (pattern, not actual code)
let _guard = claim_span.enter();  // Guard created BEFORE await
let claim = claimer.claim_one(...).await;  // AWAIT - guard still held
// Guard dropped here, but possibly on different thread!
```

**Why It Failed:**
1. Tokio can resume a future on a different worker thread after an await
2. `Entered` guards mutate a **thread-local** span stack
3. Drop on thread B cannot remove the entry left on thread A's stack
4. Span stack grows unbounded: 1 leaked entry per cycle
5. tracing's fmt layer re-serializes entire stack on every event
6. Quadratic growth: 18 deep → 2,488 deep, 4,983 bytes → 629,829 bytes per line
7. Result: ~159 GB/hr, filled 444 GB disk

**The Fix:**
Use `.instrument()` instead of manual guards:
```rust
// NEW CORRECT CODE (actual pattern from codebase)
let claim = claimer
    .claim_one(...)
    .instrument(claim_span.clone())  // Attach to future
    .await?;
```

**Why It Works:**
- `.instrument()` stores the span in the future's state
- Tracing runtime automatically enters/exits the span around each poll
- Works correctly regardless of which thread the future runs on
- No manual guard management, no thread-local corruption

---

## Recommendations

### ✅ Current State: No Changes Needed

The claim path is using the correct patterns throughout:
1. All async operations use `.instrument()` 
2. All `.enter()` guards are scoped to synchronous regions only
3. All `Span::current()` calls resolve to properly instrumented spans

### 📋 Maintenance Guidelines

When adding new span operations to the claim path (or any async path):

1. **For async code:** Always use `.instrument(your_span)`
   ```rust
   async_operation().instrument(span).await?
   ```

2. **For synchronous bookkeeping:** May use `.enter()` AFTER all awaits
   ```rust
   result = async_op().await?;
   let _guard = span.enter();  // Safe: no more awaits
   do_sync_work();
   ```

3. **Never:** Hold an `.enter()` guard across an await point
   ```rust
   let _guard = span.enter();
   result = async_op().await?;  // ❌ WRONG: guard held across await
   ```

4. **Recording attributes:** Use `Span::current()` in instrumented futures
   ```rust
   // Inside a function called via .instrument()
   tracing::Span::current().record("key", value);
   ```

---

## Appendix: Complete File Reference

**Files audited:**
- `src/claim/mod.rs` - Core claim logic
- `src/worker/mod.rs` - Claim orchestration
- `src/strand/mod.rs` - Strand evaluation (calls claim)
- `src/span/mod.rs` - Span utilities (ScopeGuard, etc.)

**Related ADRs:**
- ADR-015: Concurrent same-repo worker isolation (mentions span safety)

**Related beads:**
- `bf-3uj6i`: Original span leak bug fix
- `needle-029fca1e`: This audit

---

## Summary by Risk Category

| Category | Count | Percentage |
|----------|-------|------------|
| LIFO-safe | 31 | 100% |
| Potential-await-risk | 0 | 0% |
| Needs-fix | 0 | 0% |
| **Total** | **31** | **100%** |

**Audit Result:** ✅ **PASS** - All span operations in the claim path are safe.
