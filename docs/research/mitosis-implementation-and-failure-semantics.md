# Mitosis Implementation and Failure Semantics

**Research Date:** 2026-08-15  
**Purpose:** Document current Mitosis implementation, failure handling, and timeout context availability for enhanced timeout analysis capability.

## Overview

Mitosis is NEEDLE's bead-splitting mechanism that decomposes failed beads into focused children. It operates via two distinct code paths:

1. **Ordinary Failure Mitosis** - triggered after regular task failures
2. **Timeout-Triggered Mitosis** - triggered after productive timeouts

Both paths share the same core evaluation logic but differ in:
- **Eligibility gates** (failure count vs timeout eligibility)
- **Prompt templates** (basic vs timeout-specific)
- **Context provided to agent** (no timing info vs rich timeout context)

## Prompt Templates

### Ordinary Failure Mitosis Template (`mitosis`)

**Location:** `src/prompt/mod.rs:78-114`

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

You must output ONLY a JSON object (no markdown fencing, no explanation).

If the bead describes multiple independent tasks that can be worked on separately:
{"splittable": true, "children": [{"title": "Short task title", "body": "Task description and acceptance criteria"}, ...]}

If the bead describes a single task (even if complex or long):
{"splittable": false}

### Rules for splitting

- Split ONLY if the bead asks for MORE THAN ONE independent unit of work
- Each child must be independently completable and closable
- Valid split: "add endpoint AND write migration AND update tests" (three deliverables)
- Invalid split: bead is long, bead failed, bead has many acceptance criteria for one task
- Preserve the original acceptance criteria by distributing them to the appropriate child
- Each child title should be concise and start with a verb
- Do not propose children that duplicate any existing children listed above
```

**Key characteristics:**
- No timing context provided
- No distinction between completed vs remaining work
- Binary decision: splittable or not
- Existing children listed for deduplication only

### Timeout-Triggered Mitosis Template (`mitosis-timeout`)

**Location:** `src/prompt/mod.rs:116-199`

```
## Timeout Mitosis Analysis

This bead **timed out** after substantial productive work. Your job is to
analyze what was accomplished and decompose the REMAINING WORK into independently
closable phases.

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
   - What work cannot be safely recovered from the partial execution?

2. **Propose child beads for the REMAINING WORK ONLY:**
   - Each child must be independently completable (no dependencies on timeout-prone work)
   - Each child must be closable even if earlier phases in the original task timed out
   - Focus on deliverables that can be completed within normal time limits

3. **Refuse decomposition if appropriate:**
   - If the bead represents a single atomic task that cannot be safely divided
   - If the timeout was caused by infrastructure failure (not productive work)
   - If splitting would require redoing work that may have completed partially
   - If child tasks would require unsafe overlap with incomplete work

### Output Format

You must output ONLY a JSON object (no markdown fencing, no explanation).

**If the remaining work CAN be decomposed into independent phases:**
{"splittable": true, "children": [{"title": "Phase 1: remaining work", "body": "Description of what remains and acceptance criteria"}, ...]}

**If the bead is atomic (cannot be safely divided) OR the timeout was not productive:**
{"splittable": false, "reason": "Brief explanation why decomposition is inappropriate"}
```

**Key characteristics:**
- Rich timeout context provided (elapsed time, activity evidence)
- Distinguishes completed vs remaining work
- Requires reason field for non-splittable decisions
- Validates safety of decomposition (no unsafe overlap)

## Failure Type Handling Logic

### Classification System

Timeouts are classified by origin and activity evidence:

**TimeoutOrigin** (`src/mitosis/timeout_eligibility.rs:59-87`)
- `AgentWallclock` - GNU timeout exit code 124, legitimate task duration
- `HandlerTimeout` - Outcome handler timeout on validation gates
- `BeadStoreTimeout` - bf/br CLI timeout (never qualifies)
- `OutcomeProcessingTimeout` - Infrastructure timeout (never qualifies)

**ActivityEvidence** (`src/mitosis/timeout_eligibility.rs:89-115`)
- `HasToolUseCalls` - Tool markers in stderr (strongest evidence)
- `HasStructuredOutput` - Non-empty stdout
- `SubstantialElapsedTime` - Duration >= 90% of timeout
- `NoEvidence` - Empty stdout/stderr (idle agent)

### Eligibility Decision Tree

**Function:** `classify_timeout_eligibility()` in `src/mitosis/timeout_eligibility.rs:155-332`

```
1. Check exit code:
   - 130/143 (SIGINT/SIGTERM) → NotEligible (interrupted)
   - 0-125 (success/failure) → NotEligible (not a timeout)
   - 124 (timeout) → Continue

2. Check policy enablement:
   - !policy.enabled → NotEligible (feature disabled)

3. Check elapsed fraction:
   - elapsed_fraction < policy.min_elapsed_fraction → NotEligible (too early)

4. Classify timeout origin:
   - Exit code 124 → AgentWallclock
   - stderr contains "bead store" → BeadStoreTimeout
   - stderr contains "validation gate" → HandlerTimeout

5. Detect activity evidence:
   - stderr contains tool markers → HasToolUseCalls
   - stdout non-empty → HasStructuredOutput
   - Otherwise → NoEvidence

6. Apply policy rules:
   AgentWallclock:
   - !policy.agent_wallclock_timeout → NotEligible
   - NoEvidence → NotEligible (idle agent)
   - HasToolUseCalls | HasStructuredOutput → Eligible

   HandlerTimeout:
   - !policy.handler_timeout → NotEligible
   - SubstantialElapsedTime → Eligible
   - Otherwise → NotEligible

   BeadStoreTimeout | OutcomeProcessingTimeout → NotEligible
```

## Code Paths

### Ordinary Failure Mitosis

**Entry Point:** `src/worker/mod.rs:2711`

```rust
// Triggered on Outcome::Failure
if handler_result.outcome == Outcome::Failure {
    self.mitosis_evaluator.evaluate(
        self.store.as_ref(),
        &bead,
        &workspace,
        &self.dispatcher,
        &self.prompt_builder,
        &self.config.agent.default,
    ).await
}
```

**Implementation:** `src/mitosis/mod.rs:190` - `MitosisEvaluator::evaluate()`

**Preconditions checked:**
1. `config.enabled` - mitosis must be enabled
2. `detects_needle_internal_config()` - skip NEEDLE-internal config references
3. `current_depth < config.max_depth` - enforce depth limit
4. Failure count logic:
   - If `force_failure_threshold > 0`: require `failure_count >= threshold`
   - If `first_failure_only`: require `failure_count == 1` or repeat interval tick
   - If `repeat_interval > 0`: allow periodic retries

**Prompt built:** `build_mitosis()` - uses `DEFAULT_MITOSIS_TEMPLATE`

**Timeout context:** None (no timing information provided)

### Timeout-Triggered Mitosis

**Entry Point:** `src/worker/mod.rs:2812`

```rust
// Triggered on Outcome::Timeout
if handler_result.outcome == Outcome::Timeout {
    let duration = self.last_effort
        .as_ref()
        .map(|effort| effort.cycle_start.elapsed())
        .unwrap_or(Duration::from_secs(0));

    let agent_outcome = crate::types::AgentOutcome {
        exit_code: 124,
        stdout: String::new(),
        stderr: String::new(),
    };

    self.mitosis_evaluator.evaluate_timeout(
        self.store.as_ref(),
        &bead,
        &workspace,
        &self.dispatcher,
        &self.prompt_builder,
        &self.config.agent.default,
        &agent_outcome,
        duration,
    ).await
}
```

**Implementation:** `src/mitosis/mod.rs:434` - `MitosisEvaluator::evaluate_timeout()`

**Preconditions checked:**
1. Eligibility gate: `classify_timeout_eligibility()` - comprehensive timeout analysis
2. Same checks as ordinary: `config.enabled`, depth limit, internal config detection

**Prompt built:** `build_mitosis_timeout()` - uses `DEFAULT_MITOSIS_TIMEOUT_TEMPLATE`

**Timeout context:** Rich context provided (see below)

## Timeout Context Availability

### Context Fields

Timeout-specific prompts receive the following context:

```rust
pub struct MitosisTimeoutContext<'a> {
    pub elapsed_duration: &'a str,      // "59m"
    pub timeout_duration: &'a str,      // "1h"
    pub elapsed_percent: &'a str,       // "98%"
    pub activity_evidence: &'a str,     // "Agent emitted tool-use calls"
}
```

### Activity Evidence Generation

**Function:** `build_activity_evidence_description()` in `src/mitosis/mod.rs:1049-1091`

```rust
fn build_activity_evidence_description(outcome: &AgentOutcome) -> String {
    let mut evidence = Vec::new();

    // Check stdout for structured output
    if !outcome.stdout.is_empty() {
        evidence.push("Agent produced structured output before timeout");
    }

    // Check stderr for tool-use calls
    if !outcome.stderr.is_empty() {
        let tool_markers = [
            "tool_use:", "tool_use_id", "useagent", "useread",
            "usebash", "uselsp", "useedit", "usewrite", "<invoke>", "tool_result",
        ];

        if tool_markers.iter().any(|m| stderr_lower.contains(m)) {
            evidence.push("Agent emitted tool-use calls (active work)");
        }
    }

    if evidence.is_empty() {
        evidence.push("No clear activity evidence (empty stdout/stderr)");
    }

    evidence.join("; ")
}
```

### Duration Formatting

**Function:** `format_duration()` in `src/mitosis/mod.rs:1016-1040`

```rust
fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs >= 3600 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}
```

**Examples:**
- 300s → "5m"
- 3540s → "59m"
- 3660s → "1h 1m"

## Configuration

### TimeoutTriggeredPolicy

**Location:** `src/config/mod.rs`

```rust
pub struct TimeoutTriggeredPolicy {
    /// Whether timeout-triggered mitosis is enabled (default: false)
    pub enabled: bool,

    /// Qualify agent-level wall-clock timeouts (exit code 124)
    pub agent_wallclock_timeout: bool,

    /// Qualify outcome handler timeouts (validation gates)
    pub handler_timeout: bool,

    /// Minimum fraction of timeout budget that must elapse (0.0-1.0, default: 0.9)
    pub min_elapsed_fraction: f64,
}
```

**Default values:**
- `enabled: false` - opt-in feature
- `agent_wallclock_timeout: false`
- `handler_timeout: false`
- `min_elapsed_fraction: 0.9` (90%)

### Example Configuration

```yaml
mitosis:
  enabled: true
  first_failure_only: true
  max_depth: 0
  timeout_triggered:
    enabled: true
    agent_wallclock_timeout: true
    handler_timeout: false
    min_elapsed_fraction: 0.9
```

## Failure Semantics Summary

### Ordinary Failures
- **Trigger:** `Outcome::Failure` (non-zero exit code excluding timeout)
- **Eligibility:** Based on failure count (first_failure_only, force_failure_threshold)
- **Context:** Basic bead context only
- **Goal:** Split multi-task beads into focused children

### Timeouts
- **Trigger:** `Outcome::Timeout` (exit code 124)
- **Eligibility:** Comprehensive timeout analysis (origin + activity + elapsed fraction)
- **Context:** Rich timing context + activity evidence
- **Goal:** Decompose remaining work after productive timeout, refuse unsafe splits

### Infrastructure Failures (Never Qualify)
- **Interrupted:** Signals 130/143 - graceful shutdown
- **Crash:** Signals 137/9 - process killed
- **BeadStoreTimeout:** CLI timeouts - no agent activity
- **OutcomeProcessingTimeout:** Infrastructure delays

## Key Files

- **Core logic:** `src/mitosis/mod.rs`
- **Timeout eligibility:** `src/mitosis/timeout_eligibility.rs`
- **Prompt templates:** `src/prompt/mod.rs`
- **Configuration:** `src/config/mod.rs`
- **Worker integration:** `src/worker/mod.rs:2711` (failure), `2812` (timeout)

## Related Documentation

- ADR-015: Concurrent Same-Repo Worker Isolation (mitosis context)
- Timeout configuration integration tests: `tests/timeout_config_integration.rs`
