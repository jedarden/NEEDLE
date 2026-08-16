# Timeout Mitosis Decomposition Design

## Overview

This document specifies the design for timeout-triggered mitosis decomposition in NEEDLE. It defines the decision tree for when to split vs. refuse decomposition, the API boundary between timeout mode and ordinary failure mode, and the safety guarantees for timeout-based bead splitting.

## Motivation

Ordinary mitosis triggers on failure count thresholds (e.g., `first_failure_only`, `repeat_interval`) and splits multi-task beads into independent sub-tasks. Timeout-triggered mitosis is fundamentally different:

1. **Timeout evidence**: The bead timed out after substantial productive work
2. **Partial completion**: Git state shows some work was committed or is in progress
3. **Phase awareness**: Children represent sequential phases that can close independently
4. **Safety boundary**: Must distinguish between productive work vs. infrastructure hangs

## Decomposition Decision Tree

The timeout-triggered mitosis decision tree is a series of gates that determine whether a bead qualifies for decomposition. Each gate must be passed for the evaluation to proceed.

### Gate 1: Outcome Classification (Hard Rejection)

**Question**: Is this actually a timeout?

```rust
match Outcome::classify(exit_code, was_interrupted) {
    Outcome::Timeout => continue,
    Outcome::Interrupted => REJECT ("interrupted by signal, not a timeout"),
    Outcome::Crash(code) => REJECT ("process killed by signal {code}"),
    Outcome::Success | Outcome::Failure | Outcome::AgentNotFound => {
        REJECT ("exit code {code} is not a timeout")
    }
}
```

**Rejection rationale**: Non-timeout outcomes represent different failure modes. Interrupted/Crash indicate infrastructure issues, not productive work that exceeded time budgets.

### Gate 2: Policy Enablement (Configuration)

**Question**: Is timeout-triggered mitosis enabled in configuration?

```rust
if !policy.enabled {
    REJECT ("timeout-triggered mitosis is disabled in configuration")
}
```

**Rejection rationale**: Operators can disable timeout-triggered mitosis globally if it's not appropriate for their workflow (e.g., short-running tasks, no long-running agents).

### Gate 3: Timeout Origin Classification (Infrastructure Filter)

**Question**: Where did the timeout originate?

```rust
match classify_timeout_origin(outcome, duration) {
    TimeoutOrigin::AgentWallclock { .. } => continue,
    TimeoutOrigin::HandlerTimeout { gate_name } => continue if policy.handler_timeout,
    TimeoutOrigin::BeadStoreTimeout => REJECT ("bead-store timeout (no agent activity evidence)"),
    TimeoutOrigin::OutcomeProcessingTimeout => REJECT ("outcome-processing timeout (infrastructure slowness)"),
}
```

**Rejection rationale**: Bead-store and outcome-processing timeouts represent infrastructure slowness, not agent work. Handler timeouts qualify only if enabled in policy and the validation gate represents substantial work.

### Gate 4: Elapsed Fraction Threshold (Flaky Timeout Filter)

**Question**: Did the timeout occur after substantial elapsed time?

```rust
elapsed_fraction = duration / timeout_duration
if elapsed_fraction < policy.min_elapsed_fraction {  // default: 0.9 (90%)
    REJECT ("insufficient elapsed fraction ({elapsed_fraction} < {threshold}) — likely a flaky early timeout")
}
```

**Rejection rationale**: Timeouts that occur early (e.g., 5 minutes into a 1-hour timeout) are likely infrastructure hiccups, not genuine long-running work. The 90% threshold ensures the agent used nearly its full time budget.

### Gate 5: Activity Evidence (Idle Agent Filter)

**Question**: Was the agent actively working before the timeout?

```rust
match detect_activity_evidence(outcome) {
    ActivityEvidence::HasToolUseCalls => QUALIFIES,
    ActivityEvidence::HasStructuredOutput => QUALIFIES,
    ActivityEvidence::SubstantialElapsedTime { .. } => QUALIFIES,
    ActivityEvidence::NoEvidence => REJECT ("no evidence of agent activity (empty stdout/stderr)"),
}
```

**Rejection rationale**: A timeout with empty stdout/stderr likely occurred on an idle or stuck agent. Tool-use calls (stderr) and structured output (stdout) are affirmative evidence of work.

### Gate 6: NEEDLE Internal Configuration (Out-of-Scope Filter)

**Question**: Does the bead reference NEEDLE-internal configuration?

```rust
if detects_needle_internal_config(bead) {
    REJECT ("bead references NEEDLE-internal configuration (out of scope for target workspace)")
}
```

**Rejection rationale**: Beads that investigate NEEDLE's own dispatch configuration (Pluck, exclude_labels, etc.) have no legitimate resolution path from inside a target repo. These must be handled by NEEDLE operators, not target workspace agents.

### Gate 7: Mitosis Depth Limit (Depth Check)

**Question**: Has the bead exceeded maximum mitosis generation depth?

```rust
current_depth = parse_mitosis_depth(bead)
if policy.max_depth > 0 && current_depth >= policy.max_depth {
    REJECT ("depth {current_depth} exceeds maximum depth {policy.max_depth}")
    // Side effect: add 'human' label to flag for operator attention
}
```

**Rejection rationale**: Recursive splitting can create arbitrarily deep cascades. The `max_depth` configuration prevents infinite recursion and flags beads that require manual decomposition.

### Gate 8: Agent Decomposition Analysis (Safety Judgment)

**Question**: Does the agent judge the bead safe to split?

```rust
let proposal = dispatch_agent(build_mitosis_timeout_prompt(...))

if proposal.is_splittable() {
    ACCEPT (create children from proposal.phases)
} else {
    REJECT (proposal.refusal_reason())
}
```

**Rejection rationale**: The agent performs the final safety assessment, analyzing the bead's title/body and timeout context to determine whether decomposition is safe. The agent can refuse for:

- **Atomic tasks**: "Full test suite cannot be decomposed without rerunning from incomplete state"
- **Unsafe overlap**: "Splitting would require redoing work from an incomplete intermediate state"
- **Infrastructure failure**: "Timeout was caused by network hang, not productive computation"

## Refusal Reasons (Canonical)

The `TimeoutRefusalReason` enum captures all canonical refusal reasons for timeout-triggered mitosis:

| Variant | When Used | Example |
|---------|-----------|---------|
| `Atomic` | Task cannot be meaningfully decomposed | "Full test suite requires rerunning from scratch" |
| `UnsafeOverlap` | Splitting would redo incomplete work | "Migration must complete before subsequent tests run" |
| `InfrastructureFailure` | Timeout caused by infrastructure issue | "Network hang caused timeout, not computation" |
| `InsufficientContext` | Cannot determine safe boundary | "Git state shows no commits to infer completion" |
| `DepthLimit` | Exceeded maximum generation depth | "Depth 3 exceeds max_depth 2" |
| `OutOfScope` | Bead references NEEDLE-internal config | "Bead investigates Pluck configuration" |

## API Boundary: Timeout Mode vs. Ordinary Mode

The `MitosisMode` enum and `DecompositionProposal` enum provide a type-safe boundary between timeout-triggered and ordinary failure mitosis.

### MitosisMode

```rust
pub enum MitosisMode {
    Timeout,   // Timeout-triggered: uses timeout context, produces phases
    Ordinary,  // Ordinary failure: uses task analysis, produces sub-tasks
}
```

### DecompositionProposal

```rust
pub enum DecompositionProposal {
    Timeout {
        phases: Vec<TimeoutPhaseProposal>,
        refusal_reason: Option<String>,
    },
    Ordinary {
        tasks: Vec<OrdinaryTaskProposal>,
        refusal_reason: Option<String>,
    },
}
```

### Entry Points

| Mode | Evaluator Entry Point | Context Used | Child Type |
|------|----------------------|--------------|------------|
| `Timeout` | `MitosisEvaluator::evaluate_timeout()` | `TimeoutDecompositionContext` (elapsed time, activity evidence, git state) | `TimeoutPhaseProposal` (phase-based, independently closable) |
| `Ordinary` | `MitosisEvaluator::evaluate()` | Bead title/body only | `OrdinaryTaskProposal` (task-based, all must complete) |

### Prompt Differences

**Timeout prompt** includes:
- Elapsed duration as % of timeout budget
- Activity evidence (tool-use calls, structured output)
- Git state: pre-dispatch SHA, post-attempt SHA, committed work summary, dirty paths
- Trace file references (stdout/stderr)
- Phase structure guidance: "Phase N: [completed/pending] ..."

**Ordinary prompt** includes:
- Bead title and body only
- Existing children titles (for dedup)
- Multi-task detection guidance: "AND", "also", "additionally"

### Child Bead Labels

**Timeout mode** children receive:
- `mitosis-child`
- `mitosis-depth:N`
- `parent-{parent_id}`
- `root-{root_id}`
- `stitch:*` (inherited from parent for HOOP Hook 4)
- `phase-completed` or `phase-pending` (distinguishes closed vs open phases)

**Ordinary mode** children receive:
- `mitosis-child`
- `mitosis-depth:N`
- `parent-{parent_id}`
- `root-{root_id}`
- `stitch:*` (inherited from parent)

## Safety Decomposition Assessment

The `DecompositionSafety` type captures the analysis of whether splitting is safe:

```rust
pub enum DecompositionSafety {
    Safe {
        confidence: f32,      // 0.0 to 1.0
        evidence: Vec<String>, // Supporting evidence markers
    },
    Unsafe {
        reason: TimeoutRefusalReason,
    },
}
```

### Confidence Levels

| Confidence | Interpretation |
|------------|----------------|
| 1.0 (100%) | High-confidence safe split (e.g., git commits show clear phase boundaries) |
| 0.7-0.9 | Moderate confidence (e.g., dirty paths indicate incomplete work) |
| 0.5-0.7 | Low confidence (e.g., only elapsed fraction suggests progress) |
| 0.0 | Unsafe (refusal reason provided) |

## Phase Proposal Structure

### TimeoutPhaseProposal

```rust
pub struct TimeoutPhaseProposal {
    pub title: String,                    // "Phase 1: Complete OAuth implementation"
    pub description: String,              // Detailed acceptance criteria
    pub is_completed: bool,               // true if phase already done (closed bead)
    pub depends_on_phases: Vec<String>,   // Linearized phase dependencies
    pub estimated_duration_secs: Option<u64>,
    pub completion_criteria: Vec<String>, // Conditions for closing this phase
}
```

**Example**:

```json
{
  "title": "Phase 1: Complete OAuth token endpoint",
  "description": "Implement the /oauth/token endpoint with refresh token support. Acceptance criteria: endpoint returns 200 on valid credentials, 401 on invalid.",
  "is_completed": false,
  "depends_on_phases": [],
  "estimated_duration_secs": 1800,
  "completion_criteria": [
    "POST /oauth/token returns 200 with valid grant",
    "Refresh token is stored and validated",
    "Unit tests for token validation pass"
  ]
}
```

### OrdinaryTaskProposal

```rust
pub struct OrdinaryTaskProposal {
    pub title: String,         // "Add endpoint" (no phase number)
    pub description: String,   // Sub-task deliverables
    pub is_completed: bool,    // true if already done
}
```

**Example**:

```json
{
  "title": "Add REST endpoint",
  "description": "Create POST /api/resource endpoint",
  "is_completed": false
}
```

## Telemetry Events

Timeout-triggered mitosis emits distinct telemetry events:

| Event | Fields |
|-------|--------|
| `MitosisEvaluated` | `bead_id`, `mode: "timeout"`, `splittable: bool`, `proposed_children: u32` |
| `MitosisSplit` | `parent_id`, `mode: "timeout"`, `children_created: u32`, `children_skipped: u32`, `child_ids: Vec<BeadId>` |
| `MitosisSkipped` | `parent_id`, `mode: "timeout"`, `reason: String` |
| `MitosisOutOfScope` | `bead_id`, `mode: "timeout"` |
| `TimeoutDecompositionRefused` | `bead_id`, `refusal_reason: String`, `confidence: f32` |

## Implementation Notes

### Concurrency Safety

Timeout-triggered mitosis uses the same flock-based serialization as ordinary mitosis:

```rust
let lock_path = lock_dir.join(format!(
    "needle-mitosis-timeout-{}.lock",
    sanitize_path_component(&workspace.display().to_string())
));
let _lock = acquire_flock(&lock_path).await?;
```

The lock file name includes `-timeout-` to distinguish it from ordinary mitosis locks, preventing contention between the two modes.

### Deduplication

Timeout-triggered mitosis uses the same lineage-wide dedup as ordinary mitosis:

1. Read parent's direct children (via `parent-{parent_id}` label)
2. Read all beads in the same lineage (via `root-{root_id}` label)
3. Combine both sets for dedup against proposed phases
4. Use `titles_match()` fuzzy matching for semantic duplicates

### Git State Capture

The `TimeoutDecompositionContext` captures Git state via `capture_post_attempt_git_state()`:

```rust
pub struct PostAttemptGitState {
    pub head_sha: Option<String>,
    pub dirty_paths: BTreeSet<String>,
    pub committed_work: Option<CommittedWorkSummary>,
}
```

This enables the agent to infer phase boundaries from actual work progress:
- **Committed work**: Suggests completed phases (closed beads)
- **Dirty paths**: Suggests in-progress phases (open beads)
- **Empty state**: Suggests no progress (all phases open)

### Phase Linearization

Timeout phases are linearized by dependency order:

```rust
// Phase 1 has no dependencies
// Phase 2 depends on Phase 1
// Phase 3 depends on Phase 2
for (i, phase) in phases.iter().enumerate() {
    if i > 0 {
        phase.depends_on_phases.push(phases[i-1].title.clone());
    }
}
```

This ensures sequential execution: Phase N cannot start until Phase N-1 completes. The parent bead is blocked by all phases, closing only when all children complete.

## Testing Strategy

### Unit Tests

- `test_timeout_gate_outcome_classification`: Rejects non-timeout outcomes
- `test_timeout_gate_elapsed_fraction`: Rejects early timeouts (< 90%)
- `test_timeout_gate_activity_evidence`: Rejects empty stdout/stderr
- `test_timeout_gate_internal_config`: Rejects NEEDLE-internal config beads
- `test_timeout_gate_depth_limit`: Rejects beads exceeding max_depth
- `test_phase_proposal_serialization`: Round-trip JSON encoding

### Integration Tests

- `test_timeout_mitosis_e2e`: Full timeout-triggered mitosis flow with mock agent
- `test_phase_deduplication`: Phase proposals dedup against existing children
- `test_git_state_inference`: Agent uses committed work to infer completed phases
- `test_timeout_refusal_reasons`: All refusal reasons emit correct telemetry

### Property Tests

- `test_decomposition_safety_monotonicity`: Higher confidence should correlate with more evidence
- `test_phase_linearization_acyclic`: Phase dependencies should never form cycles
- `test_elapsed_fraction_threshold`: Threshold changes should produce monotonic eligibility

## Future Enhancements

### Machine Learning Safety Classification

Current safety assessment relies on agent judgment. Future versions could train a classifier on historical timeout mitosis outcomes to predict safe decomposition:

```rust
pub struct MlSafetyModel {
    model: trained_model::TimeoutMitosisClassifier,
}

impl MlSafetyModel {
    pub fn assess_safety(&self, context: &TimeoutDecompositionContext) -> DecompositionSafety {
        let features = extract_features(context);
        let prediction = self.model.predict(features);
        // ...
    }
}
```

### Adaptive Timeout Thresholds

Current `min_elapsed_fraction` is a static configuration (0.9). Future versions could adapt this threshold per workspace based on historical timeout patterns:

```rust
pub struct AdaptiveThreshold {
    workspace_mean: f64,
    workspace_stddev: f64,
}

impl AdaptiveThreshold {
    pub fn threshold_for_workspace(&self, workspace: &Path) -> f64 {
        // Use Z-score to determine outlier timeouts
        self.workspace_mean - 2.0 * self.workspace_stddev
    }
}
```

### Phase Parallelization

Current phases are strictly linearized. Future versions could identify parallelizable phases:

```rust
pub struct PhaseProposal {
    pub parallel_with: Vec<String>,  // Phases that can run concurrently
}
```

This would enable faster execution for independent work streams (e.g., Phase 2A and Phase 2B could run in parallel after Phase 1 completes).

## References

- ADR-015: Concurrent Same-Repo Worker Isolation (mitosis flock design)
- ADR-014: Need for Needle-Internal Config Detection (out-of-scope filter)
- `src/mitosis/timeout_eligibility.rs`: Timeout eligibility decision tree
- `src/mitosis/timeout_context.rs`: Timeout decomposition context capture
- `src/mitosis/mod.rs`: Mitosis evaluator implementation
