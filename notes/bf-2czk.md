# Test Analysis: repeat_interval_skips_max_depth_beads

## Task Completion Summary

Located and analyzed the `repeat_interval_skips_max_depth_beads` test in the NEEDLE codebase.

## Test Location

**File:** `/home/coding/NEEDLE/src/mitosis/mod.rs`
**Lines:** 1203-1244
**Test function:** `mitosis::tests::repeat_interval_skips_max_depth_beads`

## Test Purpose

This test verifies that **mitosis child beads are excluded from repeat interval re-evaluation**, preventing infinite recursion in the bead splitting mechanism.

## Background Concepts

### Mitosis System
Mitosis is the process where a failed multi-task bead is split into focused child beads. When a bead fails repeatedly, an AI agent analyzes whether it contains multiple independent tasks and creates child beads for each subtask.

### repeat_interval Mechanism
When `repeat_interval > 0`, mitosis fires at specific failure count intervals:
- `failure_count == 1` (first failure)
- `failure_count == 1 + N` (first repeat tick)
- `failure_count == 1 + 2N` (second repeat tick)
- etc.

For example, with `repeat_interval: 50`:
- Triggers at: 1, 51, 101, 151, ...
- Does NOT trigger at: 2-50, 52-100, 102-150, ...

### mitosis-depth Label
Child beads created via mitosis receive labels:
- `mitosis-child` - marks the bead as a mitosis product
- `mitosis-depth:1` - indicates first-generation child (depth = 1)
- `parent-<parent_id>` - tracks lineage for deduplication

## Test Implementation

### Configuration
```rust
let config = MitosisConfig {
    enabled: true,
    first_failure_only: false,
    force_failure_threshold: 0,
    repeat_interval: 50,  // Triggers at 1, 51, 101, ...
};
```

### Test Scenario
The test creates a bead with:
- `failure-count:51` (at a repeat tick: `(51-1) % 50 == 0`)
- `mitosis-depth:1` label (child bead marker)

### Expected Behavior
The test asserts that `MitosisResult::Skipped` is returned, confirming that:
1. The bead is at a repeat tick (`failure_count = 51`)
2. The bead has a `mitosis-depth:1` label
3. The mitosis evaluation should skip this bead despite being at a repeat tick

### Core Logic Being Tested
From `src/mitosis/mod.rs:124-134`:
```rust
let should_fire = if self.config.repeat_interval > 0 {
    let has_mitosis_depth_label =
        bead.labels.iter().any(|l| l.starts_with("mitosis-depth:"));
    let is_repeat_tick =
        failure_count > 1 && (failure_count - 1) % self.config.repeat_interval == 0;

    // Fire at first failure OR at repeat interval ticks (if not a mitosis child)
    failure_count == 1 || (is_repeat_tick && !has_mitosis_depth_label)
} else { ... };
```

**Key condition:** `!has_mitosis_depth_label` prevents repeat mitosis on children.

## Why This Matters

### Preventing Runaway Recursion
Without this check:
1. Parent bead fails 51 times → mitosis splits it into children
2. Child beads get `mitosis-depth:1` label
3. Child bead fails 51 times → mitosis would split it again (grandchildren)
4. This could continue indefinitely, creating exponentially many beads

The test verifies that step 3 is **blocked** - child beads are never re-evaluated for mitosis.

## Duplicate Test Note

There appears to be a duplicate test:
- `repeat_interval_skips_mitosis_depth_beads` (lines 1112-1150)
- `repeat_interval_skips_max_depth_beads` (lines 1203-1244)

Both tests are **identical in logic** - they:
- Set `repeat_interval: 50`
- Create a bead with `failure-count:51` and `mitosis-depth:1`
- Assert that the result is `Skipped`

This redundancy may be intentional (defensive testing) or a test maintenance artifact.

## Related Code Paths

1. **Entry point:** `MitosisEvaluator::evaluate()` (line 86)
2. **Repeat interval logic:** Lines 124-154
3. **Label check:** Line 127-128 (`has_mitosis_depth_label`)
4. **Skip path:** Lines 147-152 (returns `MitosisResult::Skipped`)
5. **Child creation:** Lines 257-364 (only for non-skipped beads)

## Test Status

The test is currently passing as of recent runs (2026-07-06), confirming the mitosis depth guard is functioning correctly.
