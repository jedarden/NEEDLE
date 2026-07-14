# Excluded/Assigned Edge Case Verification

**Task:** bf-6te3t - Verify excluded/assigned edge case handling
**Date:** 2026-07-14
**Status:** Complete

## Summary

Successfully created and verified a comprehensive test for the **excluded/assigned edge case** - beads that are **BOTH** excluded (have blocked/deferred/human labels) AND assigned (have an assignee).

## Background

The workspace iteration fix in `src/strand/explore.rs` (lines 276-280) implements defensive filtering:

```rust
candidates.retain(|b| {
    let assignee_ok = b.assignee.is_none();
    let labels_ok = !b.labels.iter().any(|l| filters.exclude_labels.contains(l));
    assignee_ok && labels_ok
});
```

This filtering removes beads that are:
1. Assigned (`assignee != None`)
2. Have excluded labels (`blocked`, `deferred`, `human`)

## Existing Tests (Prior to bf-6te3t)

Three existing deadlock tests verified the fix:

1. **`test_deadlock_multi_workspace_with_excluded_first_workspace`** - Tests excluded labels but NOT assigned
2. **`deadlock_scenario_assigned_beads_allow_advancement`** - Tests assigned beads but NOT excluded labels
3. **`deadlock_scenario_excluded_beads_allow_advancement`** - Tests blocked label but NOT assigned

**Missing:** A test for beads that are **BOTH excluded AND assigned**.

## Implementation

### New Test: `deadlock_scenario_excluded_and_assigned_beads_allow_advancement`

Location: `src/strand/explore.rs` lines 962-1057

**Scenario:**
- Workspace 1: Returns beads that are BOTH assigned AND excluded
  - 3 beads: each has an assignee AND a blocked/deferred/human label
- Workspace 2: Returns 1 valid unassigned candidate

**Expected Behavior:**
1. Strand queries workspace 1
2. Defensive filtering removes all 3 beads (doubly-unclaimable)
3. Strand advances to workspace 2
4. Returns valid candidate from workspace 2

**Test Coverage:**
- ✅ Verifies both workspaces are queried
- ✅ Verifies workspace 2's candidate is returned
- ✅ Verifies candidate is unassigned
- ✅ Verifies candidate has no excluded labels
- ✅ Prevents deadlock by confirming advancement past workspace 1

### New Mock Factory: `ExcludedAndAssignedMockFactory`

Location: `src/strand/explore.rs` lines 1260-1320

Creates stores for the excluded/assigned scenario:
- Workspace 1 → `ExcludedAndAssignedStore` (returns doubly-unclaimable beads)
- Workspace 2 → `ValidBeadStore` (returns valid unassigned bead)

### New Mock Store: `ExcludedAndAssignedStore`

Location: `src/strand/explore.rs` lines 1641-1738

Returns 3 beads that are BOTH:
1. Assigned to different workers
2. Have different excluded labels (blocked, deferred, human)

Beads returned:
- `ws1-both-1`: Assigned to "other-worker-1", labeled "blocked"
- `ws1-both-2`: Assigned to "other-worker-2", labeled "deferred"
- `ws1-both-3`: Assigned to "other-worker-3", labeled "human"

## Test Results

### Unit Test Results

```bash
$ cargo test --lib strand::explore::tests::deadlock_scenario_excluded_and_assigned_beads_allow_advancement
running 1 test
test strand::explore::tests::deadlock_scenario_excluded_and_assigned_beads_allow_advancement ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1237 filtered out
```

### All Deadlock Tests

```bash
$ cargo test --lib strand::explore::tests::deadlock
running 3 tests
test strand::explore::tests::deadlock_scenario_assigned_beads_allow_advancement ... ok
test strand::explore::tests::deadlock_scenario_excluded_and_assigned_beads_allow_advancement ... ok
test strand::explore::tests::deadlock_scenario_excluded_beads_allow_advancement ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 1235 filtered out
```

### Full Explore Strand Test Suite

```bash
$ cargo test --lib strand::explore::tests
running 18 tests
... (all 18 tests pass)

test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 1220 filtered out
```

## Verification Summary

✅ **Edge case test passes** - The new test verifies that beads which are both excluded AND assigned are correctly filtered out
✅ **No deadlock occurs** - The strand advances past workspace 1 to workspace 2
✅ **Fix handles correctly** - The defensive filtering logic (lines 276-280) handles doubly-unclaimable beads
✅ **No infinite loops** - All tests complete without hanging
✅ **No regressions** - All 18 existing tests still pass

## Acceptance Criteria Met

- ✅ **Edge case test passes without deadlock** - New test completes successfully
- ✅ **Fix correctly handles beads that are both excluded and assigned** - Defensive filtering removes beads failing EITHER condition
- ✅ **No infinite loops or hangs occur** - All tests complete instantly

## Technical Details

### Why This Edge Case Matters

The defensive filtering uses a **logical AND**:
```rust
assignee_ok && labels_ok
```

A bead is filtered out if:
- `assignee_ok = false` (has an assignee)
- OR `labels_ok = false` (has excluded labels)

A bead that is **BOTH** excluded AND assigned:
- Fails `assignee_ok` check
- Fails `labels_ok` check
- Is **definitely** filtered out

This edge case tests that the filtering logic correctly handles beads that fail **BOTH** conditions, not just one.

### Deadlock Prevention Mechanism

The fix prevents deadlock through:

1. **Filtering before checking** (lines 276-280)
   - Removes unclaimable candidates BEFORE checking if empty
   
2. **Empty check after filtering** (line 286)
   - `if candidates.is_empty()` only true AFTER filtering
   
3. **Advancement to next workspace** (line 382)
   - `continue` statement moves to next workspace when no valid candidates

This ensures that even if workspace 1 returns candidates, if ALL are filtered out (whether assigned, excluded, or BOTH), the strand advances to workspace 2 instead of returning `NoWork` prematurely.

## References

- Implementation: `src/strand/explore.rs` lines 272-376
- New test: `src/strand/explore.rs` lines 962-1057
- Mock factory: `src/strand/explore.rs` lines 1260-1320
- Mock store: `src/strand/explore.rs` lines 1641-1738
- Design document: `docs/design/workspace-iteration-fix.md`
