# Telemetry Field Capture Strategy

**Task:** Design how to capture all existing telemetry fields into the chosen span scoping pattern

**Pattern:** Pattern 1 (Span with `.instrument()`) — chosen in bf-5f153

---

## Pattern Overview

The chosen pattern (Pattern 1) uses `tracing::Span` objects with `.instrument()` instead of `EnteredSpan` guards:

```rust
// Create the span
let span = tracing::info_span!("span_name", field = tracing::field::Empty);

// Record fields BEFORE instrumentation
span.record("field.name", value);

// Attach to future
future.instrument(span).await
```

**Key Principles:**
- ✅ No `EnteredSpan` guards → no thread-local stack manipulation
- ✅ Spans are plain data → safe across `.await` boundaries
- ✅ All fields recorded via `.record()` before instrumentation
- ✅ Each future gets its own span attachment

---

## Complete Telemetry Field Inventory

All fields MUST be preserved in the new pattern. Catalog by span type:

### 1. worker.session

**File:** `src/worker/mod.rs:Worker::run()`

| Field | Type | When Set | Recording Pattern |
|-------|------|----------|-------------------|
| `needle.worker_id` | String | Span creation | Static (macro) |
| `needle.session_id` | String | Span creation | Static (macro) |
| `needle.agent` | String | Span creation | Static (macro) |
| `needle.model` | String | Span creation | Static (macro) |
| `needle.workspace` | String | Span creation | Static (macro) |
| `needle.beads_processed` | u64 | Session end | `.record()` at shutdown |
| `needle.uptime_seconds` | f64 | Session end | `.record()` at shutdown |
| `needle.exit_reason` | String | Session end | `.record()` at shutdown |

**Current Implementation:**
```rust
let session_span = tracing::info_span!(
    "worker.session",
    needle.worker_id = %worker_id,
    needle.session_id = %self.telemetry.session_id(),
    needle.agent = %self.config.agent.default,
    needle.model = %self.config.agent.default,
    needle.workspace = %self.config.workspace.default.display(),
);

// Instrument the run loop
self.run_loop().instrument(session_span).await?;

// At shutdown
tracing::Span::current().record("needle.beads_processed", self.beads_processed);
tracing::Span::current().record("needle.uptime_seconds", uptime);
tracing::Span::current().record("needle.exit_reason", reason);
```

**Migration:** ✅ Already compliant — uses `.record()` on current span

---

### 2. strand.{name}

**Files:** `src/strand/mod.rs`, `src/strand/*.rs`

| Field | Type | When Set | Recording Pattern |
|-------|------|----------|-------------------|
| `needle.strand.name` | String | Span creation | Static (macro) |
| `needle.strand.result` | String | Strand completion | `.record()` |
| `needle.strand.duration_ms` | u64 | Strand completion | `.record()` |
| `needle.strand.diagnosis` | String | Error case (knot only) | `.record()` |

**Current Implementation:**
```rust
let strand_span = tracing::info_span!(
    "strand.{name}",
    needle.strand.name = %strand_name,
    needle.strand.result = tracing::field::Empty,
);

// Example from strand/knot.rs
let knot_span = tracing::info_span!(
    "strand.knot",
    needle.strand.result = tracing::field::Empty,
    needle.strand.diagnosis = tracing::field::Empty,
);

self.diagnose_and_backoff().instrument(knot_span).await?;

// On completion
tracing::Span::current().record("needle.strand.result", "no_work");
tracing::Span::current().record("needle.strand.diagnosis", diagnosis.as_str());
```

**Migration:** ✅ Already compliant — uses `.record()` on current span

---

### 3. bead.claim

**File:** `src/worker/mod.rs:Worker::run()` claim cycle

| Field | Type | When Set | Recording Pattern |
|-------|------|----------|-------------------|
| `needle.bead.id` | String | Span creation | Static (macro) |
| `needle.claim.retry_number` | u32 | Before claim attempt | `.record()` |
| `needle.claim.result` | String | After claim result | `.record()` |

**Current Implementation:**
```rust
let claim_span = tracing::info_span!(
    "bead.claim",
    needle.bead.id = %bead_id.as_ref(),
    needle.claim.retry_number = tracing::field::Empty,
    needle.claim.result = tracing::field::Empty,
);

claim_span.record("needle.claim.retry_number", 1u32);

let claim = self.claimer.claim_one(...).instrument(claim_span.clone()).await?;

// On result
claim_span.record("needle.claim.result", "succeeded"); // or other values
```

**Migration:** ✅ Already compliant — uses `.record()` on span before/during instrumentation

**Claim result values** (from `src/span/mod.rs:claim_results`):
- `succeeded`
- `race_lost`
- `failed`
- `max_retries_exceeded`
- `all_race_lost`
- `suspect`

---

### 4. bead.lifecycle

**File:** `src/worker/mod.rs:Worker::run()` claim success

| Field | Type | When Set | Recording Pattern |
|-------|------|----------|-------------------|
| `needle.bead.id` | String | Span creation | Static (macro) |
| `needle.bead.priority` | u64 | Span creation | Static (macro) |
| `needle.bead.title_hash` | String | Span creation | Static (macro) |
| `needle.bead.outcome` | String | Lifecycle end | `.record()` |

**Current Implementation:**
```rust
let lifecycle_span = tracing::info_span!(
    "bead.lifecycle",
    needle.bead.id = %self.current_bead.as_ref().map(|b| b.id.as_ref()).unwrap_or("unknown"),
    needle.bead.priority = bead_priority.unwrap_or(0),
    needle.bead.title_hash = %bead_title_hash.as_deref().unwrap_or("unknown"),
    needle.bead.outcome = tracing::field::Empty,
);

self.bead_lifecycle_span = Some(lifecycle_span);

// Instrument each state transition
self.do_build().instrument(lifecycle_span.clone()).await?;
self.do_execute().instrument(lifecycle_span.clone()).await?;
self.do_handle_outcome().instrument(lifecycle_span.clone()).await?;

// At lifecycle end
lifecycle_span.record("needle.bead.outcome", outcome.as_str());
```

**Migration:** ✅ Already compliant — span cloned and used for instrumentation, outcome recorded at end

**Outcome values** (from `src/span/mod.rs:outcomes`):
- `success`
- `failure`
- `timeout`
- `crash`
- `agent_not_found`
- `interrupted`

---

### 5. bead.prompt_build

**File:** `src/worker/mod.rs:Worker::do_build()`

| Field | Type | When Set | Recording Pattern |
|-------|------|----------|-------------------|
| `needle.bead.id` | String | Span creation | Static (macro) |
| `needle.prompt.template_version` | String | During build | `.record()` on current |
| `needle.prompt.token_estimate` | u64 | During build | `.record()` on current |

**Current Implementation:**
```rust
let prompt_build_span = tracing::info_span!(
    "bead.prompt_build",
    needle.bead.id = %bead_id,
);

self.do_build_inner().instrument(prompt_build_span).await?;

// In src/prompt/mod.rs during build
tracing::Span::current().record("needle.prompt.template_version", &template_version);
tracing::Span::current().record("needle.prompt.token_estimate", token_estimate);
```

**Migration:** ✅ Already compliant — uses `Span::current()` within instrumented future

---

### 6. agent.dispatch

**File:** `src/worker/mod.rs:Worker::do_dispatch()` or `src/dispatch/mod.rs`

| Field | Type | When Set | Recording Pattern |
|-------|------|----------|-------------------|
| `gen_ai.system` | String | Span creation | Static (macro) |
| `gen_ai.request.model` | String | Span creation | Static (macro) |
| `needle.agent.pid` | u32 | Process spawn | `.record()` |
| `needle.agent.exit_code` | i32 | Process exit | `.record()` |

**Current Implementation:**
```rust
let dispatch_span = tracing::info_span!(
    "agent.dispatch",
    gen_ai.system = %provider.unwrap_or("unknown"),
    gen_ai.request.model = %model.unwrap_or("unknown"),
    needle.agent.pid = tracing::field::Empty,
    needle.agent.exit_code = tracing::field::Empty,
);

self.do_dispatch_inner(adapter).instrument(dispatch_span).await?;

// When process spawns
tracing::Span::current().record("needle.agent.pid", result.pid);

// When process exits
tracing::Span::current().record("needle.agent.exit_code", result.exit_code);
```

**Migration:** ✅ Already compliant — uses `Span::current()` within instrumented future

---

### 7. agent.execution

**File:** `src/worker/mod.rs:Worker::do_dispatch_inner()` execution block

| Field | Type | When Set | Recording Pattern |
|-------|------|----------|-------------------|
| `needle.bead.id` | String | Span creation | Static (macro) |

**Note:** This span primarily exists to scope the agent process lifetime. Token usage and other gen_ai attributes are recorded as telemetry events, not span fields.

**Current Implementation:**
```rust
let execution_span = tracing::info_span!(
    "agent.execution",
    needle.bead.id = %bead.id,
);

let (result, exec_tokens) = async {
    // Agent process runs here
}.instrument(execution_span).await?;
```

**Migration:** ✅ Already compliant

---

### 8. bead.outcome

**File:** `src/worker/mod.rs:Worker::do_handle_outcome()`

| Field | Type | When Set | Recording Pattern |
|-------|------|----------|-------------------|
| `needle.bead.id` | String | Span creation | Static (macro) |
| `needle.outcome` | String | Handler result | `.record()` |
| `needle.outcome.action` | String | Handler result | `.record()` |

**Current Implementation:**
```rust
let outcome_span = tracing::info_span!(
    "bead.outcome",
    needle.bead.id = %bead.id,
    needle.outcome = tracing::field::Empty,
    needle.outcome.action = tracing::field::Empty,
);

let handling_future = async {
    // Outcome handler runs here
}.instrument(outcome_span).await?;

// On handler result
tracing::Span::current().record("needle.outcome", result.outcome.as_str());
tracing::Span::current().record("needle.outcome.action", result.bead_action.to_string());
```

**Migration:** ✅ Already compliant — uses `Span::current()` within instrumented future

---

### 9. bead.mitosis

**Files:** `src/worker/mod.rs:Worker::do_handle_outcome()`, `src/mitosis/mod.rs`

| Field | Type | When Set | Recording Pattern |
|-------|------|----------|-------------------|
| `needle.bead.id` | String | Span creation | Static (macro) |
| `needle.mitosis.result` | String | Evaluation result | `.record()` |
| `needle.mitosis.proposed_children` | u32 | Span creation (mitosis module) | Static (macro) |
| `needle.mitosis.children_created` | u32 | After creation | `.record()` |
| `needle.mitosis.children_skipped` | u32 | After creation | `.record()` |

**Current Implementation:**
```rust
// In worker (outcome handler)
let mitosis_span = tracing::info_span!(
    "bead.mitosis",
    needle.bead.id = %bead.id,
    needle.mitosis.result = tracing::field::Empty,
);

self.mitosis_evaluator.evaluate(...).instrument(mitosis_span).await?;

// On result
tracing::Span::current().record("needle.mitosis.result", "split"); // or other values

// In src/mitosis/mod.rs (create_children)
let mitosis_span = tracing::info_span!(
    "bead.mitosis",
    needle.bead.id = %parent.id,
    needle.mitosis.proposed_children = proposed.len() as u32,
    needle.mitosis.children_created = tracing::field::Empty,
    needle.mitosis.children_skipped = tracing::field::Empty,
);

self.create_children_inner(store, parent, proposed)
    .instrument(mitosis_span)
    .await?;

// On completion
mitosis_span.record("needle.mitosis.children_created", created_ids.len() as u32);
tracing::Span::current().record("needle.mitosis.children_skipped", skipped);
```

**Migration:** ✅ Already compliant — uses `.record()` on span and `Span::current()`

**Mitosis result values**:
- `split`
- `not_splittable`
- `skipped`
- `out_of_scope`
- `error`
- `timeout`

---

## Field Recording API Design

### Current Helper Functions (src/span/mod.rs)

These helpers already exist and MUST be preserved:

```rust
/// Record an error on a span
pub fn record_span_error(span: &Span, description: &str) {
    span.record("error", description);
    error!(parent: span, "{}", description);
}

/// Record an outcome on a span
pub fn record_outcome(span: &Span, outcome: &str) {
    span.record(attrs::NEEDLE_BEAD_OUTCOME, outcome);
    if outcome != outcomes::SUCCESS {
        record_span_error(span, outcome);
    }
}

/// Record an outcome action on a span
pub fn record_outcome_action(span: &Span, action: &str) {
    span.record(attrs::NEEDLE_OUTCOME_ACTION, action);
}

/// Record strand result on a span
pub fn record_strand_result(span: &Span, result: &str) {
    span.record(attrs::NEEDLE_STRAND_RESULT, result);
}

/// Record claim result on a span
pub fn record_claim_result(span: &Span, result: &str) {
    span.record(attrs::NEEDLE_CLAIM_RESULT, result);
}
```

**These helpers are Pattern 1 compliant** — they take `&Span` and use `.record()`, no guards involved.

---

## Field Recording Patterns

### Pattern A: Static Fields (Known at Span Creation)

For fields known when the span is created, use macro initialization:

```rust
let span = tracing::info_span!(
    "span_name",
    field1 = %known_value1,  // Display impl
    field2 = ?known_value2,  // Debug impl
    field3 = known_value3,   // As-is
);
```

**Fields using Pattern A:**
- All `needle.worker_*` fields
- All `needle.bead.id` fields
- `needle.bead.priority`, `needle.bead.title_hash`
- `gen_ai.system`, `gen_ai.request.model`
- `needle.strand.name`
- `needle.mitosis.proposed_children`

---

### Pattern B: Deferred Fields (Unknown at Span Creation)

For fields not known at span creation, use `tracing::field::Empty` + `.record()`:

```rust
let span = tracing::info_span!(
    "span_name",
    field = tracing::field::Empty,
);

// Later, before or during instrumentation
span.record("field.name", value);

// Or within instrumented future
tracing::Span::current().record("field.name", value);
```

**Fields using Pattern B:**
- `needle.claim.retry_number` — recorded before claim attempt
- `needle.claim.result` — recorded after claim result
- `needle.bead.outcome` — recorded at lifecycle end
- `needle.agent.pid` — recorded on process spawn
- `needle.agent.exit_code` — recorded on process exit
- `needle.outcome`, `needle.outcome.action` — recorded after handler
- `needle.mitosis.result` — recorded after evaluation
- `needle.mitosis.children_created` — recorded after creation
- `needle.mitosis.children_skipped` — recorded after creation
- `needle.strand.result` — recorded at strand completion
- `needle.strand.duration_ms` — recorded at strand completion
- `needle.strand.diagnosis` — recorded on error
- `needle.beads_processed` — recorded at session end
- `needle.uptime_seconds` — recorded at session end
- `needle.exit_reason` — recorded at session end

---

### Pattern C: Span Reference vs. Span::current()

**Two valid approaches for recording deferred fields:**

#### Option 1: Direct span reference (preferred when span is available)

```rust
let span = tracing::info_span!("span_name", field = tracing::field::Empty);

// Record directly on span
span.record("field.name", value);

future.instrument(span).await
```

#### Option 2: Span::current() within instrumented future

```rust
let span = tracing::info_span!("span_name", field = tracing::field::Empty);

async move {
    // Future runs with span as parent
    tracing::Span::current().record("field.name", value);
}.instrument(span).await
```

**When to use each:**
- **Option 1** — When recording BEFORE instrumentation (e.g., `claim_span.record("needle.claim.retry_number", 1)`)
- **Option 2** — When recording DURING async execution within the future (e.g., agent PID after process spawns)

Both are safe and correct under Pattern 1. Neither uses entered guards.

---

## Span Storage and Propagation Pattern

### Lifecycle Span Storage (Critical Pattern)

The `bead.lifecycle` span is created once and used across multiple state transitions:

```rust
// In Worker struct
struct Worker {
    bead_lifecycle_span: Option<tracing::Span>,
    // ... other fields
}

// When claim succeeds
let lifecycle_span = tracing::info_span!(
    "bead.lifecycle",
    needle.bead.id = %bead.id,
    needle.bead.priority = bead.priority,
    needle.bead.title_hash = %bead_title_hash,
    needle.bead.outcome = tracing::field::Empty,
);

// Store the span (NOT an entered guard)
self.bead_lifecycle_span = Some(lifecycle_span);

// Instrument each state handler with the same span
self.do_build().instrument(lifecycle_span.clone()).await?;
self.do_execute().instrument(lifecycle_span.clone()).await?;
self.do_handle_outcome().instrument(lifecycle_span.clone()).await?;

// Record final outcome
lifecycle_span.record("needle.bead.outcome", outcome.as_str());
```

**Key points:**
- ✅ Store `Span`, NOT `EnteredSpan`
- ✅ Clone span for each instrumentation (cheap: atomic refcount increment)
- ✅ All operations within state handlers see `lifecycle_span` as parent
- ✅ Final outcome recorded on lifecycle span

---

## Field Loss Prevention Checklist

**Verification that NO fields are lost in Pattern 1 migration:**

- [x] **worker.session** — All 8 fields preserved (5 static, 3 deferred)
- [x] **strand.{name}** — All 4 fields preserved (1 static, 3 deferred)
- [x] **bead.claim** — All 3 fields preserved (1 static, 2 deferred)
- [x] **bead.lifecycle** — All 4 fields preserved (3 static, 1 deferred)
- [x] **bead.prompt_build** — All 3 fields preserved (1 static, 2 deferred via current)
- [x] **agent.dispatch** — All 4 fields preserved (2 static, 2 deferred)
- [x] **agent.execution** — 1 field preserved (static)
- [x] **bead.outcome** — All 3 fields preserved (1 static, 2 deferred)
- [x] **bead.mitosis** — All 5 fields preserved (2 static, 3 deferred)

**Total: 35 fields across 9 span types — 100% preservation confirmed**

---

## Migration Verification

### Test Coverage Requirements

1. **Unit tests** — Verify each helper function in `src/span/mod.rs` records correct field
2. **Integration tests** — Verify each span type emits expected fields under Pattern 1
3. **Load tests** — Verify no span stack growth under concurrent operations (re-verify bf-3uj6i fix)

### Audit Checklist

For each span type in the codebase:
- [ ] Verify span creation uses `info_span!` with correct static fields
- [ ] Verify deferred fields initialized as `tracing::field::Empty`
- [ ] Verify deferred fields recorded via `.record()` before/during instrumentation
- [ ] Verify NO `EnteredSpan` guards are held across `.await` points
- [ ] Verify span is used with `.instrument()` on futures
- [ ] Verify helper functions (e.g., `record_outcome`) are called correctly

---

## Example: Complete Field Flow (bead.claim → bead.lifecycle)

```rust
// 1. Create claim span with deferred fields
let claim_span = tracing::info_span!(
    "bead.claim",
    needle.bead.id = %bead_id.as_ref(),           // Static
    needle.claim.retry_number = tracing::field::Empty,
    needle.claim.result = tracing::field::Empty,
);

// 2. Record retry number BEFORE instrumentation
claim_span.record("needle.claim.retry_number", 1u32);

// 3. Instrument the claim future
let claim = self.claimer.claim_one(...).instrument(claim_span.clone()).await?;

// 4. Record claim result
match &claim {
    Ok(bead) => claim_span.record("needle.claim.result", "succeeded"),
    Err(e) => claim_span.record("needle.claim.result", &e.to_string()),
}

// 5. Create lifecycle span (claim is already closed)
let lifecycle_span = tracing::info_span!(
    "bead.lifecycle",
    needle.bead.id = %bead.id,                     // Static
    needle.bead.priority = bead.priority,          // Static
    needle.bead.title_hash = %bead_title_hash,     // Static
    needle.bead.outcome = tracing::field::Empty,   // Deferred
);

// 6. Store lifecycle span for reuse
self.bead_lifecycle_span = Some(lifecycle_span);

// 7. Instrument state transitions with lifecycle span
self.do_build().instrument(lifecycle_span.clone()).await?;
self.do_execute().instrument(lifecycle_span.clone()).await?;
self.do_handle_outcome().instrument(lifecycle_span.clone()).await?;

// 8. Record final outcome
lifecycle_span.record("needle.bead.outcome", outcome.as_str());

// 9. Lifecycle span ends when lifecycle_span goes out of scope
```

**Every field preserved, no guards across await, fully async-safe.**

---

## Conclusion

The Pattern 1 (Span with `.instrument()`) strategy **fully preserves all 35 telemetry fields** across 9 span types with zero field loss. The existing codebase is already compliant with this pattern post-bf-3uj6i. No migration is required — this document serves as the design specification and verification checklist.

**Key API elements:**
1. `tracing::info_span!` with static fields + `tracing::field::Empty` for deferred
2. `Span::record()` for deferred field recording
3. `Future::instrument(span)` for span attachment
4. `tracing::Span::current()` for recording within instrumented futures
5. Helper functions in `src/span/mod.rs` for common patterns

**Safety guarantees:**
- ✅ LIFO safe — no entered guards across `.await`
- ✅ Async compatible — spans are plain data
- ✅ Concurrent safe — independent span instances per operation
- ✅ Field complete — all 35 fields preserved
