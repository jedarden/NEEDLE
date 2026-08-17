# Timeout Decomposition Types and Interfaces Design

**Bead:** `needle-cdcc8041` (Child 2 of 4 for `bf-18cy6`)  
**Author:** NEEDLE Mitosis Subsystem  
**Status:** Design Complete  
**Created:** 2026-08-16

## Overview

This document specifies the types, interfaces, and decision boundaries for timeout-triggered mitosis in NEEDLE. When an agent times out after substantial productive work, the system must intelligently decompose the remaining work into independently closable phases rather than retrying the original task.

**Design Goal:** Enable safe decomposition of timeout-prone beads into phases that can be completed and closed independently, even if earlier phases in the original task timed out.

---

## 1. Timeout-Specific Mitosis Analysis Result Types

### 1.1 Core Analysis Result

The primary type for timeout-triggered mitosis is **already defined** in `src/mitosis/mod.rs`:

```rust
/// Agent's mitosis analysis response for timeout decomposition.
#[derive(Debug, serde::Deserialize)]
struct MitosisResponse {
    /// Whether the remaining work can be decomposed into independent phases.
    splittable: bool,
    
    /// Proposed child beads for remaining work (only when splittable=true).
    #[serde(default)]
    children: Vec<ProposedChild>,
    
    /// Required explanation for refusal (only when splittable=false).
    /// Explains WHY decomposition is inappropriate for this timeout.
    #[serde(default)]
    reason: Option<String>,
}
```

**Key Design Decision:** The `reason` field is **mandatory for timeout mode** when `splittable=false`. Unlike ordinary failure mitosis (where refusal may not require explanation), timeout decomposition refusal must explain why the timeout cannot be safely split.

### 1.2 Child Bead Proposal Structure

The **child bead proposal** is defined as:

```rust
/// A child bead proposed by the agent during mitosis analysis.
#[derive(Debug, Clone, serde::Deserialize)]
struct ProposedChild {
    /// Concise phase title (e.g., "Phase 1: Complete OAuth implementation")
    title: String,
    
    /// Full description including:
    /// - What work remains in this phase
    /// - Acceptance criteria for completion
    /// - Dependencies on other phases (preferably none)
    body: String,
}
```

**Design Principles for Proposed Children:**

1. **Independence:** Each child must be completable without requiring successful completion of timeout-prone earlier phases.
2. **Closability:** Each child must have clear acceptance criteria that allow it to be closed independently.
3. **Time-boxed:** Each child should be completable within normal time limits (not another timeout).
4. **No Redundancy:** Children should not require redoing work that may have partially completed before the timeout.

### 1.3 Timeout Eligibility Types

Timeout eligibility is determined by types in `src/mitosis/timeout_eligibility.rs`:

```rust
/// Eligibility decision for timeout-triggered mitosis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeoutEligibility {
    /// Timeout qualifies for mitosis — productive work that exceeded budget.
    Eligible { reason: String },
    
    /// Timeout does not qualify — not a timeout or not productive work.
    NotEligible { reason: String },
}

/// Classification of timeout origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeoutOrigin {
    /// Agent process wall-clock timeout (GNU timeout exit code 124).
    AgentWallclock { timeout_duration: Duration },
    
    /// Outcome handler timeout (validation gate exceeded budget).
    HandlerTimeout { gate_name: Option<String> },
    
    /// Bead-store timeout (never qualifies — no agent activity evidence).
    BeadStoreTimeout,
    
    /// Outcome-processing timeout (never qualifies — infrastructure).
    OutcomeProcessingTimeout,
}

/// Evidence that the agent was actively working (not idle/crashed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityEvidence {
    /// Agent emitted tool-use calls before timeout (stderr contains markers).
    HasToolUseCalls,
    
    /// Agent produced structured output before timeout (stdout non-empty).
    HasStructuredOutput,
    
    /// Substantial elapsed time (e.g., >=90% of timeout budget).
    SubstantialElapsedTime { elapsed: Duration, timeout: Duration },
    
    /// No evidence of activity — timeout may have occurred on idle agent.
    NoEvidence,
}
```

### 1.4 Timeout Context Capture

The timeout context structure for persistence is in `src/mitosis/timeout_context.rs`:

```rust
/// Captured context for a timeout that may qualify for mitosis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutDecompositionContext {
    /// When this context was captured.
    pub captured_at: DateTime<Utc>,
    
    /// Bead definition at timeout time.
    pub bead_def: BeadDefinition,
    
    /// Timeout classification and reasoning.
    pub timeout: TimeoutContext,
    
    /// Reference to agent execution trace files.
    pub trace_reference: TraceReference,
    
    /// Git state before and after the agent attempt.
    pub git_state: GitStateContext,
    
    /// Whether this qualifies for mitosis (from eligibility analysis).
    pub qualifies_for_mitosis: bool,
}
```

---

## 2. API Boundary: Timeout Mode vs. Ordinary Failure Mode

### 2.1 Entry Point Separation

The mitosis evaluator provides **two distinct entry points**:

```rust
impl MitosisEvaluator {
    /// Ordinary failure mitosis (triggered by failure count thresholds).
    pub async fn evaluate(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        workspace: &Path,
        dispatcher: &Dispatcher,
        prompt_builder: &PromptBuilder,
        agent_name: &str,
    ) -> Result<MitosisResult>;
    
    /// Timeout-triggered mitosis (triggered by timeout with productive work).
    pub async fn evaluate_timeout(
        &self,
        store: &dyn BeadStore,
        bead: &Bead,
        workspace: &Path,
        dispatcher: &Dispatcher,
        prompt_builder: &PromptBuilder,
        agent_name: &str,
        outcome: &AgentOutcome,     // ← Additional: execution result
        duration: Duration,         // ← Additional: wall-clock duration
    ) -> Result<MitosisResult>;
}
```

**Key API Differences:**

| Aspect | Ordinary Failure (`evaluate`) | Timeout Mode (`evaluate_timeout`) |
|--------|------------------------------|-----------------------------------|
| **Trigger** | Failure count thresholds | Timeout eligibility |
| **Inputs** | Bead + workspace | Bead + workspace + outcome + duration |
| **Precondition check** | `failure_count >= threshold` | `TimeoutEligibility::is_eligible()` |
| **Prompt template** | `mitosis` | `mitosis-timeout` |
| **Response requirements** | `reason` optional | `reason` **required** when not splittable |
| **Shared constraints** | Max depth, OutOfScope detection | Same + eligibility check |

### 2.2 Template Separation

The prompt builder provides **distinct templates**:

```rust
impl PromptBuilder {
    /// Ordinary failure mitosis prompt.
    pub fn build_mitosis(
        &self,
        bead: &Bead,
        workspace: &Path,
        worker_id: &str,
        existing_children: &str,
    ) -> Result<Prompt>;
    
    /// Timeout-specific mitosis prompt (includes timeout context).
    pub fn build_mitosis_timeout(
        &self,
        bead: &Bead,
        workspace: &Path,
        worker_id: &str,
        existing_children: &str,
        timeout_context: &MitosisTimeoutContext,  // ← Additional context
    ) -> Result<Prompt>;
}
```

### 2.3 Timeout Context Injection

The timeout context structure passed to the prompt:

```rust
/// Timeout-specific context for prompt construction.
pub struct MitosisTimeoutContext<'a> {
    /// Human-readable elapsed duration (e.g., "59m").
    pub elapsed_duration: &'a str,
    
    /// Human-readable timeout duration (e.g., "1h").
    pub timeout_duration: &'a str,
    
    /// Percentage of timeout budget used (e.g., "98%").
    pub elapsed_percent: &'a str,
    
    /// Description of agent activity evidence.
    pub activity_evidence: &'a str,
}
```

### 2.4 Shared Response Contract

**Both modes use the same `MitosisResponse` structure**, but with different contract requirements:

```rust
// Ordinary failure: reason is optional
{"splittable": false}  // ← Valid

// Timeout mode: reason is STRONGLY recommended
{"splittable": false, "reason": "Full test suite cannot be decomposed without rerunning from incomplete state"}  // ← Preferred
{"splittable": false}  // ← Valid but discouraged (logs warning)
```

---

## 3. Decomposition Decision Tree

### 3.1 High-Level Decision Flow

```
┌─────────────────────────────────────────────────────────────┐
│  AGENT TIMEOUT (exit code 124, timeout > 0)                  │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│  CLASSIFY TIMEOUT ORIGIN                                     │
│  ├─ AgentWallclock (GNU timeout)                             │
│  ├─ HandlerTimeout (validation gate)                         │
│  ├─ BeadStoreTimeout (never eligible)                         │
│  └─ OutcomeProcessingTimeout (never eligible)                 │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│  CHECK TIMEOUT-TRIGGERED MITOSIS ENABLED                     │
│  └─ config.timeout_triggered.enabled == true                 │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│  CHECK ELAPSED FRACTION THRESHOLD                             │
│  └─ elapsed / timeout >= min_elapsed_fraction (default 0.9) │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│  DETECT ACTIVITY EVIDENCE                                     │
│  ├─ HasToolUseCalls (stderr contains tool markers)            │
│  ├─ HasStructuredOutput (stdout non-empty)                    │
│  └─ SubstantialElapsedTime (>=90% of budget)                 │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│  TIMEOUT ELIGIBLE?                                            │
│  └─ All checks passed AND activity evidence detected         │
└─────────────────────────────────────────────────────────────┘
                    │                           │
               YES │                           │ NO
                    ▼                           ▼
┌───────────────────────────┐   ┌───────────────────────────┐
│  PROCEED TO MITOSIS        │   │  SKIP WITH REASON          │
│  ANALYSIS                  │   │  └─ Not eligible: reason   │
└───────────────────────────┘   └───────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────────────────────────────┐
│  CHECK PRECONDITIONS (shared with ordinary mitosis)          │
│  ├─ Mitosis enabled globally                                 │
│  ├─ Not NEEDLE-internal config (OutOfScope check)            │
│  └─ Not exceeded max_depth                                   │
└─────────────────────────────────────────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────────────────────────────┐
│  BUILD TIMEOUT-SPECIFIC PROMPT                               │
│  ├─ Include elapsed duration, timeout duration              │
│  ├─ Include elapsed percentage                               │
│  ├─ Include activity evidence                               │
│  └─ Instruct agent to distinguish completed vs. remaining     │
└─────────────────────────────────────────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────────────────────────────┐
│  AGENT ANALYSIS                                               │
│  ├─ Read bead body, trace files, git state                    │
│  ├─ Determine if remaining work can be decomposed            │
│  └─ Output JSON: {splittable, children?, reason?}            │
└─────────────────────────────────────────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────────────────────────────┐
│  PARSE RESPONSE                                               │
│  ├─ splittable=true + children → CREATE CHILDREN             │
│  ├─ splittable=false + reason → NOT SPLITTABLE (safe)         │
│  └─ splittable=true without children → NOT SPLITTABLE         │
└─────────────────────────────────────────────────────────────┘
```

### 3.2 Agent-Side Decomposition Decision Tree

When the agent receives the timeout mitosis prompt, it must evaluate:

```
FOR each timeout bead:
    │
    ├─ CAN I DETERMINE WHAT WAS COMPLETED?
    │   ├─ YES → Continue analysis
    │   └─ NO → REFUSE (reason: "Cannot determine completion state from available evidence")
    │
    ├─ IS THE TASK ATOMIC (cannot be safely divided)?
    │   ├─ YES → REFUSE (reason: "Task is atomic and cannot be safely divided")
    │   └─ NO → Continue to next check
    │
    ├─ DID THE TIMEOUT OCCUR DURING PRODUCTIVE WORK?
    │   ├─ NO (infrastructure failure, hang, crash) → REFUSE (reason: "Timeout caused by infrastructure failure, not productive work")
    │   └─ YES → Continue to decomposition
    │
    ├─ CAN REMAINING WORK BE COMPLETED INDEPENDENTLY?
    │   ├─ NO (depends on incomplete work) → REFUSE (reason: "Splitting would require unsafe overlap with incomplete work")
    │   └─ YES → Propose children for remaining work
    │
    └─ CAN CHILDREN BE COMPLETED WITHIN NORMAL TIME LIMITS?
        ├─ NO → REFUSE (reason: "Proposed phases would likely exceed normal time budgets")
        └─ YES → OUTPUT: {splittable: true, children: [...]}
```

### 3.3 Refusal Categories (Non-Splittable Reasons)

When `splittable=false`, the agent **must** provide a `reason` that falls into one of these categories:

| Category | Reason Pattern | Example |
|----------|---------------|---------|
| **Atomic Task** | Task cannot be safely divided | `"Full test suite cannot be decomposed without rerunning from incomplete state"` |
| **Infrastructure Failure** | No productive work evidence | `"Timeout was caused by infrastructure failure, not productive work"` |
| **Unsafe Overlap** | Children would require redoing incomplete work | `"Splitting would require unsafe overlap with incomplete work"` |
| **Unknown State** | Cannot determine what completed | `"Cannot determine completion state from available evidence"` |
| **Time Risk** | Children would likely timeout again | `"Proposed phases would likely exceed normal time budgets"` |

---

## 4. Safety Constraints and Invariants

### 4.1 Must-Hold Invariants

1. **No Redundant Work:** Children must not require redoing work that may have partially completed before the timeout.
2. **Independent Closure:** Each child must be closable with clear acceptance criteria, even if earlier phases timed out.
3. **Time-Bounded:** Each child should be completable within normal time limits (e.g., <1 hour for typical tasks).
4. **No Cross-Dependencies:** Children should not depend on the successful completion of timeout-prone earlier phases.

### 4.2 Forbidden Patterns

The following patterns **MUST NOT** appear in timeout decompositions:

❌ **"Re-run the same task with smaller scope"**  
   *Reason:* This is ordinary mitosis, not timeout-specific decomposition.  
   *Correct approach:* Analyze what completed and propose children for only the remaining work.

❌ **"Split the task in half arbitrarily"**  
   *Reason:* Arbitrary splitting may require redoing work that completed partially.  
   *Correct approach:* Identify phase boundaries based on completed deliverables.

❌ **"Add a child to 'continue where we left off'"**  
   *Reason:* This creates a dependency on incomplete work and is not independently closable.  
   *Correct approach:* Propose a child with clear acceptance criteria that doesn't require knowing the exact state of incomplete work.

---

## 5. Examples

### 5.1 Valid Timeout Decomposition

**Original Bead:**
```
Title: Implement OAuth flow with comprehensive tests
Description: 
1. Design OAuth 2.0 flow architecture
2. Implement token endpoint
3. Implement authorization endpoint
4. Write comprehensive integration tests
5. Document the flow
```

**Timeout Context:**
- Elapsed: 59m of 1h (98% used)
- Activity evidence: Agent emitted tool-use calls (implementing token endpoint)
- Git state: 1 commit "feat: add token endpoint structure"

**Agent Response:**
```json
{
  "splittable": true,
  "children": [
    {
      "title": "Phase 1: Complete authorization endpoint implementation",
      "body": "Implement the authorization endpoint as specified in the OAuth design. Acceptance: Endpoint returns valid authorization codes, handles error cases."
    },
    {
      "title": "Phase 2: Add comprehensive integration tests",
      "body": "Write integration tests covering the full OAuth flow. Tests must pass for both token and authorization endpoints. Acceptance: All tests pass, coverage >=80%."
    },
    {
      "title": "Phase 3: Document OAuth flow",
      "body": "Write API documentation for the OAuth flow. Include setup instructions, examples, and troubleshooting guide. Acceptance: Documentation complete and reviewed."
    }
  ]
}
```

**Why This Works:**
- Each phase is independently completable
- Phase 1 doesn't require redoing the token endpoint (already completed)
- Phases 2 and 3 can close successfully even if Phase 1 times out again
- Each phase has clear acceptance criteria

### 5.2 Invalid Timeout Decomposition (Atomic Task)

**Original Bead:**
```
Title: Run full integration test suite
Description: Execute the complete integration test suite and report results
```

**Timeout Context:**
- Elapsed: 45m of 1h (75% used)
- Activity evidence: Tests were running (stdout shows test progress)

**Agent Response:**
```json
{
  "splittable": false,
  "reason": "Full test suite cannot be decomposed without rerunning from incomplete state. Tests must run from a clean start to be valid."
}
```

**Why This Is Correct:**
- The task is atomic — splitting into "run half the tests" is invalid
- Partial test results are not usable
- Splitting would require rerunning tests from an incomplete state

### 5.3 Invalid Timeout Decomposition (Infrastructure Failure)

**Original Bead:**
```
Title: Deploy to production environment
Description: Deploy the application to production and verify health
```

**Timeout Context:**
- Elapsed: 30m of 1h (50% used)
- Activity evidence: No tool calls, empty output (likely infrastructure hang)

**Agent Response:**
```json
{
  "splittable": false,
  "reason": "Timeout was caused by infrastructure failure (no agent activity evidence), not productive work. Decomposition is inappropriate for this failure mode."
}
```

**Why This Is Correct:**
- No evidence of productive work (empty stdout/stderr)
- Timeout was likely due to infrastructure issues, not task execution
- Splitting wouldn't address the root cause

---

## 6. Implementation Status

As of 2026-08-16, the following components are **already implemented**:

- ✅ `TimeoutEligibility` enum and classification logic (`timeout_eligibility.rs`)
- ✅ `TimeoutDecompositionContext` persistence structure (`timeout_context.rs`)
- ✅ `MitosisResponse` with `reason` field for timeout-specific refusal
- ✅ `ProposedChild` structure for child bead proposals
- ✅ `MitosisEvaluator::evaluate_timeout()` entry point
- ✅ `PromptBuilder::build_mitosis_timeout()` with timeout context
- ✅ `DEFAULT_MITOSIS_TIMEOUT_TEMPLATE` in `src/prompt/mod.rs`
- ✅ Comprehensive tests for timeout mitosis in `src/mitosis/mod.rs`

**This design document specifies and explains the existing implementation**, providing the architectural rationale for the types and interfaces already in use.

---

## 7. Future Enhancements

Potential future improvements (not in current scope):

1. **Smart Phase Detection:** Automatically detect phase boundaries from git commits, file changes, or trace patterns.
2. **Timeout Prediction:** Predict which beads are likely to timeout based on historical patterns and proactively suggest decomposition.
3. **Progress Inference:** Infer progress from stdout/stderr patterns to better distinguish completed vs. remaining work.
4. **Child Time Estimates:** Automatically estimate time budgets for proposed children based on the parent's elapsed time and remaining work proportion.

---

## 8. References

- ADR-015: Concurrent Same-Repo Worker Isolation
- `src/mitosis/mod.rs` — Mitosis evaluator implementation
- `src/mitosis/timeout_eligibility.rs` — Timeout eligibility classification
- `src/mitosis/timeout_context.rs` — Timeout context persistence
- `src/prompt/mod.rs` — Prompt templates including `DEFAULT_MITOSIS_TIMEOUT_TEMPLATE`
- `src/config/mod.rs` — Timeout-triggered mitosis configuration
