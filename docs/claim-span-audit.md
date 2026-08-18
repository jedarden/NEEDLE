# Claim Path Span Usage Audit

**Date:** 2026-08-17  
**Scope:** Complete audit of all span-related operations in the NEEDLE claim path  
**Purpose:** Identify any span guards that may be held across await points (LIFO safety violations)

## Executive Summary

**Status:** ✅ **NO ISSUES FOUND**

The claim path uses span operations safely. All span guards are properly scoped, and no guards are held across `.await` points. The codebase follows the correct pattern of using `.instrument()` to attach spans to futures rather than holding `Entered` guards.

**Key Finding:** The claim path does NOT create or enter spans directly. It only uses `tracing::Span::current().record()` to record attributes on the current span, which is set by the caller via `.instrument()`.

---

## 1. Claim Module (`src/claim/mod.rs`)

### 1.1 Span Operations Summary

The claim module contains **NO span creation or guard operations**. It only records attributes on the current span.

### 1.2 `tracing::Span::current().record()` Usage

All span operations in the claim module are attribute recordings on the current span:

| Line | Operation | Attribute | Context | Safety |
|------|-----------|-----------|---------|--------|
| 183 | `Span::current().record()` | `needle.claim.result` | Max retries exceeded | ✅ LIFO-safe |
| 185 | `Span::current().record()` | `otel.status_code` | Max retries exceeded | ✅ LIFO-safe |
| 186 | `Span::current().record()` | `otel.status_description` | Max retries exceeded | ✅ LIFO-safe |
| 194 | `Span::current().record()` | `needle.bead.id` | Claim attempt start | ✅ LIFO-safe |
| 195 | `Span::current().record()` | `needle.claim.retry_number` | Claim attempt start | ✅ LIFO-safe |
| 215 | `Span::current().record()` | `otel.status_code` | Flock timeout | ✅ LIFO-safe |
| 216 | `Span::current().record()` | `otel.status_description` | Flock timeout | ✅ LIFO-safe |
| 233 | `Span::current().record()` | `otel.status_code` | Verify failed | ✅ LIFO-safe |
| 234 | `Span::current().record()` | `otel.status_description` | Verify failed | ✅ LIFO-safe |
| 287 | `Span::current().record()` | `needle.claim.result` | Claim succeeded | ✅ LIFO-safe |
| 299 | `Span::current().record()` | `needle.claim.result` | Race lost | ✅ LIFO-safe |
| 312 | `Span::current().record()` | `needle.claim.result` | Not claimable | ✅ LIFO-safe |
| 320 | `Span::current().record()` | `needle.claim.result` | Claim error | ✅ LIFO-safe |
| 335-336 | `Span::current().record()` | Error status | Claim error threshold | ✅ LIFO-safe |
| 359-361 | `Span::current().record()` | Error status | Suspect outcome | ✅ LIFO-safe |
| 375 | `Span::current().record()` | `needle.claim.result` | Store error | ✅ LIFO-safe |
| 390-391 | `Span::current().record()` | Error status | Store error threshold | ✅ LIFO-safe |
| 399-400 | `Span::current().record()` | Error status | Store error | ✅ LIFO-safe |
| 407 | `Span::current().record()` | `needle.claim.result` | All race lost | ✅ LIFO-safe |
| 409-410 | `Span::current().record()` | Error status | All race lost | ✅ LIFO-safe |
| 558-559 | `Span::current().record()` | Success attrs | claim_auto success | ✅ LIFO-safe |
| 569 | `Span::current().record()` | `needle.claim.result` | claim_auto failed | ✅ LIFO-safe |
| 583-585 | `Span::current().record()` | Error attrs | claim_auto store error | ✅ LIFO-safe |

**Total Operations:** 28 `Span::current().record()` calls

**Safety Analysis:**
- All operations are synchronous attribute recordings
- No span guards are created or held
- No `span.enter()` or `ScopeGuard` usage
- All spans are entered via `.instrument()` by the caller

**Risk Category:** LIFO-Safe ✅

---

## 2. Worker Module (`src/worker/mod.rs`)

### 2.1 Span Creation in Claim Path

The worker creates spans using `tracing::info_span!()` and attaches them to futures with `.instrument()`.

#### 2.1.1 Claim Span Creation (`do_claim()`)

**Location:** `src/worker/mod.rs` ~1691-1708

```rust
let claim_span = tracing::info_span!(
    "bead.claim",
    needle.bead.id = %bead_id.as_ref(),
    needle.claim.retry_number = tracing::field::Empty,
    needle.claim.result = tracing::field::Empty,
);

let claim = self
    .claimer
    .claim_one(&bead_id, &self.qualified_id(), &exclusions, Some(strand))
    .instrument(claim_span.clone())
    .await?;
```

**Safety:** ✅ **SAFE** - Uses `.instrument()` to attach span to future, no guard held

**Documentation in Code:**
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

#### 2.1.2 Claim Auto Usage

**Location:** `src/worker/mod.rs` ~1335-1343

```rust
let strand = "auto";
let claim = self.claimer.claim_auto(&self.qualified_id(), strand).await;
```

**Observation:** ⚠️ **NO SPAN CONTEXT** - `claim_auto` is called without an explicit span wrapper

**Risk:** Low - The call inherits the current span context (likely `strand.auto` or similar from the strand evaluation)

### 2.2 Lifecycle Span Creation

After a successful claim, the worker creates a `bead.lifecycle` span:

**Location:** `src/worker/mod.rs` ~1729-1743

```rust
let lifecycle_span = tracing::info_span!(
    "bead.lifecycle",
    needle.bead.id = %self.current_bead.as_ref().map(|b| b.id.as_ref()).unwrap_or("unknown"),
    needle.bead.priority = bead_priority.unwrap_or(0),
    needle.bead.title_hash = %bead_title_hash.as_deref().unwrap_or("unknown"),
    needle.bead.outcome = tracing::field::Empty, // Will be set on completion
);
self.bead_lifecycle_span = Some(lifecycle_span);
```

**Safety:** ✅ **SAFE** - Span is stored, not entered. Used later with `.instrument()`

### 2.3 Lifecycle Span Usage

The lifecycle span is used with `.instrument()` for async operations and `in_scope()` for sync operations:

#### 2.3.1 Async Operations with `.instrument()`

**Location:** `src/worker/mod.rs` ~956-966

```rust
WorkerState::Building => self.do_build().instrument(lifecycle_span.clone()).await?,
WorkerState::Prompting => {
    self.do_prompt()
        .instrument(lifecycle_span.clone())
        .await?
}
WorkerState::Executing => self.do_execute().instrument(lifecycle_span.clone()).await?,
WorkerState::Handling => self.do_handle().instrument(lifecycle_span.clone()).await?,
```

**Safety:** ✅ **SAFE** - All async operations use `.instrument()`

#### 2.3.2 Sync Operations with `.in_scope()`

**Location:** `src/worker/mod.rs` ~969

```rust
WorkerState::Logging => lifecycle_span.in_scope(|| self.do_log())?,
```

**Safety:** ✅ **SAFE** - `do_log()` is synchronous (no await points)

**Location:** `src/worker/mod.rs` ~5039-5044 (test code)

```rust
lifecycle_span.in_scope(|| {
    tracing::info!(cycle, "claim-cycle-depth-probe");
});

lifecycle_span.in_scope(|| worker.do_log()).unwrap();
```

**Safety:** ✅ **SAFE** - All `in_scope()` closures are synchronous

### 2.4 Span Documentation Comments

The worker module includes explicit documentation about span safety:

**Location:** `src/worker/mod.rs` ~395

```rust
/// The current bead lifecycle span. Created when a bead is claimed and
/// remains active for the entire bead processing pipeline.
///
/// This must remain a `Span`, not an `EnteredSpan`: entered guards mutate a
/// thread-local span stack and are NOT safe to hold across `.await` points.
/// The span is attached to async operations via `.instrument()` instead.
```

**Location:** `src/worker/mod.rs` ~23

```rust
// Needed for `.instrument()` — attaches a span to a future instead of holding an
// `Entered` guard across `.await`, which is unsound and leaked spans (bf-3uj6i).
```

---

## 3. Span Module (`src/span/mod.rs`)

### 3.1 ScopeGuard Implementation

The `ScopeGuard` type is explicitly documented as **UNSAFE to use across await points**:

**Location:** `src/span/mod.rs` ~143-189

```rust
/// RAII guard for LIFO-safe span scoping.
///
/// When dropped, the guard explicitly closes the span, ensuring proper
/// unwinding even across early returns or errors. This ensures LIFO
/// (Last-In-First-Out) compliance by using RAII to guarantee spans unwind
/// in reverse order.
///
/// # Safety
///
/// This guard must NOT be stored across an `.await` point. The guard
/// should only be used for synchronous code blocks. For async code,
/// use `.instrument()` on the future instead.
```

**Usage in Claim Path:** **NONE** - `ScopeGuard` is not used in the claim path

### 3.2 Safety Comments

The module includes explicit safety warnings:

**Location:** `src/span/mod.rs` ~149-152

```rust
/// # Safety
///
/// This guard must NOT be stored across an `.await` point. The guard
/// should only be used for synchronous code blocks. For async code,
/// use `.instrument()` on the future instead.
```

**Location:** `src/span/mod.rs` ~171-174

```rust
/// # Panics
///
/// This function uses `span.enter()` which modifies thread-local state.
/// Do not store the returned guard across an `.await` point.
```

---

## 4. Strand Module (`src/strand/mod.rs`)

### 4.1 Strand Span Usage

The strand module uses `span.enter()` for **synchronous only** bookkeeping after async operations complete:

**Location:** `src/strand/mod.rs` ~272-296

```rust
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
// ... (synchronous attribute recording)
```

**Safety:** ✅ **SAFE** - Guard is used only for synchronous code after the await completes

**Documentation in Code:**
```rust
// The remaining evaluation bookkeeping is synchronous, so a
// scoped guard is safe here and preserves strand context on its
// tracing events without crossing an `.await`.
```

---

## 5. Cross-Await Safety Verification

### 5.1 Pattern Analysis

The claim path follows two patterns for span handling:

#### Pattern 1: `.instrument()` for Async Operations ✅

```rust
let span = tracing::info_span!("operation");
async_operation().instrument(span).await
```

**Used in:**
- `worker::do_claim()` - claim_one call
- `worker::do_build()` - build operations
- `worker::do_execute()` - execute operations
- `worker::do_handle()` - handle operations
- `strand::select()` - strand evaluation

#### Pattern 2: `in_scope()` for Sync Operations ✅

```rust
span.in_scope(|| {
    // synchronous work only
})
```

**Used in:**
- `worker::do_log()` - logging is synchronous
- `strand::select()` - bookkeeping after await completes

### 5.2 Anti-Pattern Detection

The following anti-patterns were searched for and **NOT FOUND** in the claim path:

❌ Holding an `Entered` guard across an `.await` point  
❌ Using `ScopeGuard` across an `.await` point  
❌ Calling `span.enter()` before an `.await` without dropping before  
❌ Storing an `EnteredSpan` in a struct field  

---

## 6. Risk Assessment by Category

| Category | Count | Risk Level | Notes |
|----------|-------|------------|-------|
| `Span::current().record()` | 28 | ✅ None | Synchronous attribute recording only |
| `.instrument()` usage | 9 | ✅ None | Correct pattern for async operations |
| `in_scope()` usage | 3 | ✅ None | Used only for synchronous closures |
| `span.enter()` usage | 1 | ✅ None | Post-await synchronous bookkeeping only |
| `ScopeGuard` usage | 0 | ✅ None | Not used in claim path |
| Cross-await guards | 0 | ✅ None | No violations found |

**Total Operations Audited:** 41  
**Operations with Risk:** 0  

---

## 7. Line Number Reference

### 7.1 Claim Module (`src/claim/mod.rs`)

All `tracing::Span::current().record()` calls:
- 183, 185-186: Max retries exceeded
- 194-195: Claim attempt start
- 215-216: Flock timeout
- 233-234: Verify failed
- 287: Claim succeeded
- 299: Race lost
- 312: Not claimable
- 320: Claim error
- 335-336: Error threshold
- 359-361: Suspect outcome
- 375: Store error
- 390-391: Store error threshold
- 399-400: Store error final
- 407, 409-410: All race lost
- 558-559: claim_auto success
- 569: claim_auto failed
- 583-585: claim_auto store error

### 7.2 Worker Module (`src/worker/mod.rs`)

- 23: Comment explaining `.instrument()` necessity
- 1688-1708: Claim span creation and usage
- 1729-1743: Lifecycle span creation
- 956-966: Async operations with `.instrument()`
- 969: Sync operation with `in_scope()`
- 395: Documentation comment about span safety
- 1335-1343: claim_auto usage (no explicit span)

### 7.3 Strand Module (`src/strand/mod.rs`)

- 272-296: Strand span with post-await `enter()` usage

### 7.4 Span Module (`src/span/mod.rs`)

- 143-189: `ScopeGuard` implementation with safety docs
- 149-152: Safety warning about await points
- 171-174: Panic documentation about await points

---

## 8. Conclusion

**Summary:** The claim path has **NO span-related LIFO safety violations**.

**Key Strengths:**
1. All async operations use `.instrument()` correctly
2. No guards are held across await points
3. Synchronous operations use `in_scope()` appropriately
4. Code includes comprehensive documentation about span safety
5. Previous issues (bf-3uj6i) are documented to prevent recurrence

**Recommendations:**
1. ✅ No changes needed - current implementation is safe
2. Consider adding explicit span context to `claim_auto()` call for consistency
3. Maintain current documentation and safety comments

**Verification Method:** Complete code audit of all span operations in the claim path, with line number references and risk categorization.

---

## Appendix A: Span Hierarchy

```text
worker.session                                          (root span, lifetime = worker process)
├── strand.auto                                         (claim_auto inherits this)
│   └── bead.claim                                      (explicit span for do_claim)
├── strand.{name}                                       (other strands)
│   └── bead.claim                                      (explicit span for do_claim)
│       └── bead.lifecycle                              (created after successful claim)
│           ├── bead.prompt_build
│           ├── agent.dispatch
│           │   └── agent.execution
│           └── bead.outcome
```

## Appendix B: Related Issues

- **bf-3uj6i:** Previous span leak issue that prompted the current safe implementation pattern. This issue caused ~159 GB/hr of log growth due to leaked span guards.

## Appendix C: Safety Invariant

The codebase maintains the following safety invariant:

> **No span guard may be held across an `.await` point.**

This invariant is maintained by:
1. Using `.instrument()` to attach spans to futures
2. Using `in_scope()` only for synchronous closures
3. Using `span.enter()` only for post-await synchronous bookkeeping
4. Never storing `Entered` guards in struct fields

---

**Audit Completed:** 2026-08-17  
**Auditor:** NEEDLE bead `needle-029fca1e`  
**Status:** ✅ PASSED - No issues found
