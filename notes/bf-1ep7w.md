# Bead bf-1ep7w Implementation Verification

## Overview

This document verifies that all acceptance criteria for bead bf-1ep7w have been met. The implementation addresses the three compounding issues that caused Splice to miss successfully-completing-but-unresolved bead cycles (like ARMOR bead bf-34xw9).

## Acceptance Criteria Status

### ✅ 1. New Detector for Completion-Without-Resolution Pattern

**Location:** `src/strand/splice.rs:559-639`

**Implementation:** `detect_completion_without_resolution()`

**What it detects:**
- High `agent.completed` count (≥20, configurable via `claim_churn_threshold`)
- High `bead.claim.succeeded` count (≥5)
- High `bead.orphaned` count (≥5)
- **NO** `bead.completed` events (the key signal)

**Pattern detected:** A bead that completes agent runs successfully but never closes, cycling through claim → orphan → reclaim indefinitely.

**Event tracking:**
```rust
let mut claim_succeeded_counts: HashMap<String, u32> = HashMap::new();
let mut orphaned_counts: HashMap<String, u32> = HashMap::new();
let mut agent_completed_counts: HashMap<String, u32> = HashMap::new();
let mut bead_completed_seen: HashSet<String> = HashSet::new();
```

**Thresholds (chosen to catch bf-34xw9 pattern):**
- `agent_completed_count >= claim_churn_threshold` (default: 20)
- `claim_succeeded_count >= 5`
- `orphaned_count >= 5`

### ✅ 2. Config Validation for report_workspace

**Location:** `src/config/mod.rs:2213-2224`

**Implementation:** Boot-time WARN in `ConfigLoader::emit_warnings()`

**Warning message:**
```
strands.splice.enabled is true, but strands.splice.report_workspace is not set.
Splice will not create worker failure or loop detection beads.
Set strands.splice.report_workspace to a valid workspace path in your config
(e.g., ~/.config/needle/config.yaml or .needle.yaml).
```

**Behavior:**
- Emits a WARN-level log at boot time
- Does NOT fail validation (non-blocking)
- Clear guidance on how to fix the misconfiguration

**Test coverage:** `config::tests::splice_enabled_without_report_workspace_emits_warning`

### ✅ 3. Label Original Bead as Human When Stuck

**Location:** `src/strand/splice.rs:978-1022`

**Implementation:** Circuit-breaker logic in `document_live_loop()`

**What it does:**
1. Extracts `stuck_bead_id` from the detected loop pattern
2. Gets the original workspace from the worker's heartbeat
3. Verifies the workspace exists and has a `.beads/` directory
4. Instantiates a bead store for the original workspace
5. Labels the stuck bead as `"human"`

**Why this matters:**
- **Pluck** excludes `"human"`-labeled beads from selection
- **Unravel** only processes `"human"`-labeled beads
- This is the actual circuit breaker that stops retry storms

**Logging:**
```rust
tracing::info!(
    stuck_bead_id = %stuck_bead_id,
    workspace = %original_workspace,
    "splice: labeled stuck bead as human to stop redispatch"
);
```

**Error handling:** Gracefully logs warnings if workspace or bead store fails, but doesn't fail the loop documentation.

### ✅ 4. Regression Test with bf-34xw9 Telemetry Shape

**Location:** `src/strand/splice.rs:1276-1337`

**Test:** `detect_completion_without_resolution_finds_bf_34xw9_pattern()`

**Fixture data (matches actual incident):**
- 41 × `bead.claim.succeeded` events
- 25 × `bead.orphaned` events
- 42 × `agent.completed` events
- 0 × `bead.completed` events (critical signal)

**Assertions:**
```rust
assert!(result.is_some(), "Detector should find the pattern");
let info = result.unwrap();
assert_eq!(info.bead_id, "bf-34xw9");
assert_eq!(info.claim_succeeded_count, 41);
assert_eq!(info.orphaned_count, 25);
assert_eq!(info.agent_completed_count, 42);
```

**Additional tests:**
- `detect_completion_without_resolution_ignores_resolved_beads()` - Ensures resolved beads don't trigger
- `detect_completion_without_resolution_requires_minimum_thresholds()` - Tests low-count noise filtering

## How It Works End-to-End

### Detection Flow

1. **Worker heartbeat scan** (`scan_live_loops`)
   - Reads all heartbeat files from heartbeat directory
   - Skips stale heartbeats (handled by failure detector)
   - For each live worker, reads JSONL tail

2. **Pattern detection** (`check_worker_for_loops`)
   - Parses last N events from JSONL (default: 200)
   - Runs 4 detectors in sequence:
     - `detect_claim_churn` - race_lost loops
     - `detect_state_ping_pong` - state cycling
     - `detect_log_runaway` - file growth without completion
     - `detect_completion_without_resolution` - **NEW** agent.completed without bead.completed

3. **Loop documentation** (`document_live_loop`)
   - Creates side-report bead in report_workspace
   - **EXTRACTS stuck bead ID** from loop pattern
   - **LABELS stuck bead as "human"** in its original workspace
   - Returns early if report_workspace is None (with debug log)

### Circuit Breaker Behavior

**Before this fix:**
- Pluck keeps redispatching the stuck bead
- Worker completes agent runs 42 times
- Bead never closes
- No escalation (report_workspace was unset in lab)
- Unravel never sees the bead (not labeled "human")

**After this fix:**
- Splice detects the pattern within 200 events
- Creates loop bead in report_workspace
- Labels original bead as "human"
- Pluck stops redispatching (excludes "human" label)
- Unravel picks up the bead for remediation (only processes "human")

## Test Results

All tests pass:
```
test strand::splice::tests::detect_completion_without_resolution_finds_bf_34xw9_pattern ... ok
test strand::splice::tests::detect_completion_without_resolution_ignores_resolved_beads ... ok
test strand::splice::tests::detect_completion_without_resolution_requires_minimum_thresholds ... ok
test config::tests::splice_enabled_without_report_workspace_emits_warning ... ok
```

## Files Modified

- `src/strand/splice.rs` - Core detector implementation
- `src/config/mod.rs` - Config validation warning
- `tests/` embedded in splice.rs (unit tests)

## Deployment Impact

**Risk level:** LOW
- New detector only fires on clear pathological patterns
- Config validation is WARN-only (non-blocking)
- Bead labeling is graceful (logs warnings on failure)
- All existing detectors unchanged

**Monitoring:**
- Watch for `"splice: labeled stuck bead as human"` log messages
- Verify report_workspace is set in production config
- Check for new `"worker-loop"` labeled beads in report workspace

## Conclusion

All acceptance criteria for bead bf-1ep7w have been met and tested. The implementation successfully addresses the three compounding issues identified in the ARMOR bf-34xw9 incident:

1. ✅ Detector now sees claim-succeeded + orphaned + agent.completed patterns (not just race-lost)
2. ✅ Config validation emits boot-time WARN if report_workspace is unset
3. ✅ Original stuck bead is labeled "human" to stop redispatch and enable Unravel remediation
4. ✅ Regression test ensures the bf-34xw9 pattern is caught and handled

**Status:** COMPLETE - Ready for commit and bead closure.
