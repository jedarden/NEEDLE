# Mitosis Implementation and Failure Semantics Research

**Date:** 2026-08-16  
**Bead:** needle-4bbda691  
**Purpose:** Document current Mitosis implementation and failure semantics for OTLP testing improvements

---

## Overview

Mitosis is NEEDLE's bead splitting mechanism that decomposes multi-task beads into focused child beads on failure. The implementation has **two distinct evaluation paths** with different prompts, context, and eligibility requirements.

---

## Two Mitosis Evaluation Paths

### 1. Ordinary Failure Mitosis (`evaluate()`)

**Trigger:** After any `Outcome::Failure` (exit code 1, 2-123, 125-128)

**Location:** `src/worker/mod.rs:2701`

**Code Path:**
```
handle_failure() → increments failure count → mitosis_evaluator.evaluate()
```

**Prompt Template:** `DEFAULT_MITOSIS_TEMPLATE` (`src/prompt/mod.rs:78-114`)

**Eligibility Checks:**
- `mitosis.enabled` (global enablement)
- Failure count threshold checks:
  - `first_failure_only` + `failure_count == 1` OR
  - `repeat_interval` periodic re-evaluation OR
  - `force_failure_threshold` ceiling
- `max_depth` limit (adds `human` label when exceeded)
- Skip if NEEDLE-internal config detected

**Context Provided:**
- Bead title
- Bead body/description
- Existing children (for deduplication)
- **No timeout information**
- **No execution duration**
- **No activity evidence**

**Agent Instructions:**
```json
{
  "splittable": true,
  "children": [
    {"title": "Short task title", "body": "Task description and acceptance criteria"}
  ]
}
```

**Key Constraint:** Agent has NO visibility into what actually happened during the failed attempt - only the bead's static description.

---

### 2. Timeout-Triggered Mitosis (`evaluate_timeout()`)

**Trigger:** After `Outcome::Timeout` (exit code 124)

**Location:** `src/worker/mod.rs:2787`

**Code Path:**
```
handle_timeout() → adds 'deferred' label → mitosis_evaluator.evaluate_timeout()
```

**Prompt Template:** `DEFAULT_MITOSIS_TIMEOUT_TEMPLATE` (`src/prompt/mod.rs:116-199`)

**Eligibility Checks (Stricter):**
- All ordinary mitosis checks (enabled, max_depth, internal config)
- **PLUS timeout-specific gate:** `classify_timeout_eligibility()` (`src/mitosis/timeout_eligibility.rs:155-332`)
  - Exit code must be 124 (GNU timeout)
  - NOT interrupted (exit code 130/143)
  - NOT a crash (signal kills)
  - Minimum elapsed fraction (default: 90% of timeout budget)
  - Activity evidence required:
    - `HasToolUseCalls` (stderr non-empty with tool markers)
    - `HasStructuredOutput` (stdout non-empty)
    - `SubstantialElapsedTime` (fallback)

**Context Provided:**
- Bead title/body/description
- Existing children (for deduplication)
- **Timeout context:**
  - `elapsed_duration` (e.g., "59m")
  - `timeout_duration` (e.g., "1h")
  - `elapsed_percent` (e.g., "98%")
  - `activity_evidence` (description of what agent was doing)

**Agent Instructions:**
```json
{
  "splittable": true,
  "children": [
    {"title": "Phase 1: remaining work", "body": "Description of what remains and acceptance criteria"}
  ]
}
```

**With Reason Field:**
```json
{
  "splittable": false,
  "reason": "Brief explanation why decomposition is inappropriate"
}
```

**Key Difference:** Agent receives timeout context and MUST distinguish between completed work vs remaining work.

---

## Failure Type Handling

### Semantic Errors (Ordinary Failures)

**Exit Codes:** 1, 2-123, 125-128

**Handler:** `handle_failure()` (`src/outcome/mod.rs:693-761`)

**Flow:**
1. Release bead (status: open, assignee: "")
2. Increment failure count label
3. Check quarantine threshold (`quarantine_after_failures`)
4. If threshold exceeded → quarantine (status: blocked)
5. Trigger ordinary mitosis evaluation

**No special semantic error classification** - all non-timeout failures are treated identically.

---

### Timeouts

**Exit Code:** 124 (GNU timeout wrapper)

**Handler:** `handle_timeout()` (`src/outcome/mod.rs:767-811`)

**Flow:**
1. Release bead
2. Increment failure count
3. Add `deferred` label
4. Trigger timeout-triggered mitosis evaluation

**Timeout Origin Classification:** (`src/mitosis/timeout_eligibility.rs:334-370`)

| Origin | Eligibility | Reason |
|--------|------------|--------|
| `AgentWallclock` | Qualifies if enabled + activity evidence | Legitimate task duration limit |
| `HandlerTimeout` | Qualifies if enabled + substantial elapsed time | Post-agent validation gate timeout |
| `BeadStoreTimeout` | NEVER qualifies | No agent activity evidence |
| `OutcomeProcessingTimeout` | NEVER qualifies | Infrastructure slowness |

**Activity Evidence Detection:** (`src/mitosis/timeout_eligibility.rs:396-434`)

```rust
enum ActivityEvidence {
    HasToolUseCalls,      // stderr contains "tool_use:", "<invoke>", etc.
    HasStructuredOutput,  // stdout non-empty
    SubstantialElapsedTime, // >= 90% of timeout budget
    NoEvidence,           // Empty stdout/stderr
}
```

---

### Infrastructure Failures

**Handler:** `handle_crash()` (`src/outcome/mod.rs:813-889`)

**Exit Codes:** 129+ (signals), negative values

**Flow:**
1. Release bead
2. Create alert bead with diagnostic info
3. **NO mitosis evaluation** - crashes don't trigger splitting

**Signal-Specific Behavior:**
- SIGINT (130) → `Outcome::Interrupted` (clean shutdown)
- SIGTERM (143) → `Outcome::Interrupted` (clean shutdown)
- SIGKILL (137) → `Outcome::Crash` (force kill)

---

## Code Path Summary

### Ordinary Failure Analysis

```
src/worker/mod.rs:2701
├── mitosis_evaluator.evaluate()
│   ├── src/mitosis/mod.rs:190 (evaluate())
│   │   ├── Check: mitosis.enabled
│   │   ├── Check: detects_needle_internal_config()
│   │   ├── Check: max_depth limit
│   │   ├── Check: failure_count thresholds
│   │   ├── Get: existing_children (label-based discovery)
│   │   ├── Build: build_mitosis() prompt
│   │   │   └── src/prompt/mod.rs:829
│   │   │       └── DEFAULT_MITOSIS_TEMPLATE
│   │   ├── Dispatch: agent analysis
│   │   └── Create: children with dedup
│   └── Returns: MitosisResult (Split/NotSplittable/Skipped/OutOfScope)
```

### Timeout Analysis

```
src/worker/mod.rs:2787
├── Capture: duration from last_effort.cycle_start
├── Build: AgentOutcome (exit_code=124, stdout="", stderr="")
├── mitosis_evaluator.evaluate_timeout()
│   ├── src/mitosis/mod.rs:434 (evaluate_timeout())
│   │   ├── Step 1: classify_timeout_eligibility()
│   │   │   └── src/mitosis/timeout_eligibility.rs:155
│   │   │       ├── Check: exit code == 124
│   │   │       ├── Check: Outcome::classify() == Timeout
│   │   │       ├── Check: timeout_triggered.enabled
│   │   │       ├── Check: min_elapsed_fraction (default 0.9)
│   │   │       ├── Classify: timeout_origin
│   │   │       ├── Detect: activity_evidence
│   │   │       └── Returns: TimeoutEligibility (Eligible/NotEligible)
│   │   ├── Step 2: if !eligible → return Skipped
│   │   ├── Step 3-5: Same checks as ordinary mitosis
│   │   ├── Step 6-7: Gather existing_children
│   │   ├── Step 8: Build timeout context
│   │   │   ├── elapsed_duration (format_duration)
│   │   │   ├── timeout_duration (format_timeout_duration)
│   │   │   ├── elapsed_percent (calculate)
│   │   │   └── activity_evidence (build_activity_evidence_description)
│   │   ├── Step 9: build_mitosis_timeout() prompt
│   │   │   └── src/prompt/mod.rs:860
│   │   │       └── DEFAULT_MITOSIS_TIMEOUT_TEMPLATE
│   │   ├── Step 10-11: Dispatch agent, parse response
│   │   └── Returns: MitosisResult (same variants)
```

---

## Timeout Information Availability

### Where Timeout Duration is Known

**Configuration:** `src/config/mod.rs`

```rust
pub struct AgentConfig {
    pub timeout_seconds: u64,  // Default: 3600 (1 hour)
}
```

**Worker Tracking:** `src/worker/mod.rs:2802-2806`

```rust
let duration = self
    .last_effort
    .as_ref()
    .map(|effort| effort.cycle_start.elapsed())
    .unwrap_or(std::time::Duration::from_secs(0));
```

**Problem:** `EffortData` does NOT track stdout/stderr, so the agent outcome is constructed with empty strings:

```rust
let agent_outcome = crate::types::AgentOutcome {
    exit_code: 124,
    stdout: String::new(),  // EMPTY!
    stderr: String::new(),  // EMPTY!
};
```

**Impact:** Timeout eligibility classification (`classify_timeout_eligibility`) cannot detect:
- Tool-use calls in stderr
- Structured output in stdout
- Actual activity evidence

**Workaround in Current Code:** The eligibility check falls back to `SubstantialElapsedTime` if stdout/stderr are empty, but this is a weak signal - a hang on infrastructure produces the same "substantial elapsed time" signature as productive work.

---

## Mitosis Prompt Templates

### Ordinary Mitosis Template

**File:** `src/prompt/mod.rs:78-114`

**Structure:**
```
## Mitosis Analysis

Analyze the following bead and determine if it describes MULTIPLE INDEPENDENT TASKS.

### Bead

**Title:** {bead_title}
**Description:**
{bead_body}
**Bead ID:** {bead_id}

### Existing Children

{existing_children}

### Instructions

You must output ONLY a JSON object...

### Rules for splitting

- Split ONLY if the bead asks for MORE THAN ONE independent unit of work
- Each child must be independently completable and closable
...
```

**Template Variables:**
- `{bead_id}`, `{bead_title}`, `{bead_body}`
- `{existing_children}`

---

### Timeout Mitosis Template

**File:** `src/prompt/mod.rs:116-199`

**Structure:**
```
## Timeout Mitosis Analysis

This bead **timed out** after substantial productive work...

### Bead

**Title:** {bead_title}
**Description:**
{bead_body}
**Bead ID:** {bead_id}

### Timeout Context

**Elapsed Time:** {elapsed_duration} of {timeout_duration} ({elapsed_percent}% used)
**Evidence of Activity:**
{activity_evidence}

**Existing Children:**
{existing_children}

### Your Task

1. **Distinguish completed from remaining work:**
   - What progress was made before the timeout?
   - What deliverables are still incomplete?
...

### Rules for Timeout Decomposition

- **Split ONLY if:** Remaining work can be completed independently...
- **Each child must:** Be completable within normal time limits...
...
```

**Template Variables:**
- All ordinary mitosis variables PLUS:
- `{elapsed_duration}`, `{timeout_duration}`, `{elapsed_percent}`, `{activity_evidence}`

---

## Configuration

### Mitosis Configuration

**File:** `src/config/mod.rs:2293-2332`

```rust
pub struct MitosisConfig {
    pub enabled: bool,                    // Default: true
    pub first_failure_only: bool,         // Default: true
    pub force_failure_threshold: u32,     // Default: 0 (disabled)
    pub repeat_interval: u32,             // Default: 0 (disabled)
    pub max_depth: u32,                   // Default: 0 (unlimited)
    pub timeout_triggered: TimeoutTriggeredPolicy,
}
```

### Timeout-Triggered Policy

**File:** `src/config/mod.rs:2211-2291`

```rust
pub struct TimeoutTriggeredPolicy {
    pub enabled: bool,                     // Default: false (opt-in!)
    pub agent_wallclock_timeout: bool,     // Default: false
    pub handler_timeout: bool,              // Default: false
    pub min_elapsed_fraction: f64,         // Default: 0.9 (90%)
}
```

**Default Behavior:** Timeout-triggered mitosis is DISABLED by default - must be explicitly enabled in configuration.

---

## Deduplication Strategy

**Scope:** Lineage-wide dedup (not just direct children)

**Implementation:** `src/mitosis/mod.rs:686-743`

```rust
// Read parent's existing children AND all beads in the same lineage
let existing = self.get_existing_children(store, &parent.id).await?;
let lineage_beads = self.get_lineage_beads(store, &root_label).await?;

// Combine both sets for comprehensive dedup
let mut existing_titles: Vec<String> = existing.iter().map(|t| t.to_lowercase()).collect();
existing_titles.extend(lineage_beads.iter().map(|t| t.to_lowercase()));
```

**Matching Logic:** `src/mitosis/mod.rs:1099-1245`

```rust
fn titles_match(existing: &str, proposed: &str) -> bool {
    // 1. Exact match
    if existing == proposed { return true; }
    
    // 2. Substring match (fast path)
    let e = normalize(existing);
    let p = normalize(proposed);
    if e.contains(&p) || p.contains(&e) { return true; }
    
    // 3. Fuzzy Jaccard similarity (threshold: 0.6)
    // - Strip stopwords (verify, confirm, the, a, that, uses, not, ...)
    // - Tokenize on whitespace/hyphens/underscores
    // - Normalize abbreviations (pct -> percentage, calc -> calculation)
    // - Calculate |intersection| / |union|
}
```

**Regression Test:** `src/mitosis/mod.rs:1915-1939` (bf-47bll EMA calculation real-world example)

---

## Key Gaps and Limitations

### 1. No Semantic Error Classification

Ordinary failures receive NO information about:
- What actually went wrong
- Which files/functions were involved
- What the agent tried before failing
- Error messages or stack traces

**Result:** Mitosis agent works from bead description only, with no visibility into the actual failure mode.

### 2. Empty Stdout/Stderr in Timeout Analysis

**Code:** `src/worker/mod.rs:2808-2814`

```rust
let agent_outcome = crate::types::AgentOutcome {
    exit_code: 124,
    stdout: String::new(),  // EMPTY!
    stderr: String::new(),  // EMPTY!
};
```

**Impact:** Activity evidence detection (`classify_timeout_eligibility`) cannot distinguish:
- Productive work (tool calls, structured output)
- Infrastructure hang (idle agent)
- Partial completion vs nothing done

**Workaround:** Falls back to `SubstantialElapsedTime` but this is a weak proxy - both productive work and infrastructure failure produce the same "long time" signature.

### 3. No Ordinary Failure Context

The `build_mitosis()` prompt provides:
- Bead title/body
- Existing children
- **NOTHING ELSE**

No visibility into:
- Exit code (why it failed)
- Compilation errors
- Test failures
- Agent behavior before failure

**Result:** Agent cannot adapt its splitting strategy to the specific failure mode.

### 4. Timeout Mitosis is Opt-In Only

**Default:** `timeout_triggered.enabled = false`

**Reason:** Unclear whether timeouts represent productive work vs infrastructure hangs without stdout/stderr tracking.

**Impact:** Most NEEDLE deployments never use the more sophisticated timeout analysis path.

---

## File Reference Summary

### Core Implementation Files

| File | Lines | Purpose |
|------|-------|---------|
| `src/mitosis/mod.rs` | 3820 | Mitosis evaluator, dedup logic |
| `src/mitosis/timeout_eligibility.rs` | 619 | Timeout classification and eligibility |
| `src/mitosis/timeout_context.rs` | 792 | Timeout context capture and persistence |
| `src/prompt/mod.rs` | 1050+ | Prompt templates and variable substitution |
| `src/outcome/mod.rs` | 2150+ | Outcome handlers (failure, timeout, crash) |
| `src/worker/mod.rs` | 3200+ | Worker main loop, mitosis dispatch |
| `src/config/mod.rs` | 7500+ | Configuration structures |

### Key Functions by Location

| Function | File | Lines |
|----------|------|-------|
| `evaluate()` | `src/mitosis/mod.rs` | 190-407 |
| `evaluate_timeout()` | `src/mitosis/mod.rs` | 434-654 |
| `classify_timeout_eligibility()` | `src/mitosis/timeout_eligibility.rs` | 155-332 |
| `handle_failure()` | `src/outcome/mod.rs` | 693-761 |
| `handle_timeout()` | `src/outcome/mod.rs` | 767-811 |
| `build_mitosis()` | `src/prompt/mod.rs` | 829-843 |
| `build_mitosis_timeout()` | `src/prompt/mod.rs` | 860-881 |
| Ordinary mitosis dispatch | `src/worker/mod.rs` | 2701-2783 |
| Timeout mitosis dispatch | `src/worker/mod.rs` | 2787-2886 |

---

## Test Coverage

### Mitosis Tests

| Test | File | Purpose |
|------|------|---------|
| `parse_response_not_splittable` | `src/mitosis/mod.rs` | 1484-1491 |
| `parse_response_splittable_with_children` | `src/mitosis/mod.rs` | 1494-1508 |
| `parse_timeout_response_*` | `src/mitosis/mod.rs` | 1527-1624 |
| `titles_match_*` | `src/mitosis/mod.rs` | 1898-1988 |
| `create_children_with_dedup` | `src/mitosis/mod.rs` | 2113-2158 |

### Timeout Eligibility Tests

| Test | File | Purpose |
|------|------|---------|
| `eligible_agent_wallclock_timeout_with_output` | `src/mitosis/timeout_eligibility.rs` | 460-476 |
| `not_eligible_interrupted` | `src/mitosis/timeout_eligibility.rs` | 497-509 |
| `not_eligible_insufficient_elapsed_fraction` | `src/mitosis/timeout_eligibility.rs` | 527-544 |
| `not_eligible_no_activity_evidence` | `src/mitosis/timeout_eligibility.rs` | 547-564 |

### Outcome Handler Tests

| Test | File | Purpose |
|------|------|---------|
| `handle_failure_releases_and_increments_count` | `src/outcome/mod.rs` | 1516-1544 |
| `handle_timeout_releases_and_adds_deferred` | `src/outcome/mod.rs` | 1686-1704 |

---

## Conclusion

### Current State

1. **Two distinct mitosis paths** with different prompts, eligibility checks, and context availability
2. **Timeout-triggered mitosis** is more sophisticated but opt-in only (disabled by default)
3. **Ordinary failure mitosis** lacks context about what actually failed
4. **Stdout/stdr tracking gap** prevents accurate activity evidence detection for timeouts

### Implications for OTLP Testing

To properly instrument Mitosis for OTLP testing:

1. **Track stdout/stderr in `EffortData`** - Critical for timeout eligibility classification
2. **Emit spans for both evaluation paths** - Currently only "bead.mitosis" span exists
3. **Add attributes for:**
   - Eligibility decision details
   - Timeout origin classification
   - Activity evidence markers
   - Deduplication decisions
4. **Test both paths independently** - Timeout mitosis is rarely enabled in production

### Recommended Next Steps

1. Fix stdout/stderr tracking gap
2. Enable timeout-triggered mitosis in test configuration
3. Add OTLP spans to `classify_timeout_eligibility` 
4. Emit mitosis result details as span attributes
5. Add integration tests that exercise timeout-triggered path

---

**Document Version:** 1.0  
**Last Updated:** 2026-08-16  
**Next Review:** After OTLP instrumentation implementation
