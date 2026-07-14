# Explore Strand Workspace Iteration Deadlock Analysis

## Task

Analyze explore strand workspace iteration deadlock scenario (bead: bf-47tew)

## Overview

Analyzed the explore strand implementation in `src/strand/explore.rs` to understand the workspace iteration logic and identify the root cause of the deadlock scenario.

## Current Workspace Iteration Logic

The `evaluate` function (lines 215-409) iterates through configured workspaces:

```rust
for workspace in &self.workspaces {
    // Skip home workspace
    // Check .beads/ directory exists
    // Create store for workspace
    // Query for candidates and process
}
```

### Workspace Query Flow (lines 273-390)

For each workspace, the strand:

1. **Creates filters** (lines 238-245):
   ```rust
   let filters = Filters {
       assignee: None,
       exclude_labels: vec![
           "deferred".to_string(),
           "human".to_string(),
           "blocked".to_string(),
       ],
   };
   ```

2. **Queries the store** (line 273):
   ```rust
   match remote_store.ready(&filters).await {
   ```

3. **Filters by assignee** (line 276):
   ```rust
   candidates.retain(|b| b.assignee.is_none());
   ```

4. **Checks if empty** (line 278):
   ```rust
   if candidates.is_empty() {
       // Run cross-workspace mend and re-query
   }
   ```

5. **If not empty, returns candidates** (line 389):
   ```rust
   return StrandResult::BeadFound(candidates);
   ```

## The Deadlock Scenario

### Scenario Setup
- **Workspace 1**: Has beads but all have excluded labels (blocked/deferred/human)
- **Workspace 2**: Has valid unassigned beads without excluded labels
- **Expected**: Strand should advance past workspace 1 and return workspace 2's beads
- **Actual**: Strand returns NoWork without checking workspace 2

### Root Cause

**Location**: `src/strand/explore.rs` lines 273-390

**Issue**: The explore strand does **not** perform defensive label filtering after querying the store.

Compare with PluckStrand (`src/strand/pluck.rs`):
- Lines 206-221: Pluck filters out excluded labels **defensively**
- Line 221: `candidates.retain(|b| !b.labels.iter().any(|l| self.exclude_labels.contains(l)));`

The explore strand only filters by assignee (line 276) but **not** by labels.

### Why This Causes Deadlock

1. **Backend Dependency**: The strand relies on the bead store's `ready()` method to filter by labels. Different backends may implement filtering inconsistently.

2. **Mock Implementation**: The test mock `ExcludedCandidatesStore::ready()` returns all candidates regardless of filters (lines 1079-1126):
   ```rust
   async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
       // Returns candidates with blocked/deferred/human labels
       // Ignores exclude_labels in filters
   }
   ```

3. **Execution Flow**:
   - Workspace 1 query returns candidates with excluded labels
   - Candidates are NOT filtered by labels in explore strand
   - `candidates.is_empty()` is FALSE (there are candidates)
   - Strand returns `StrandResult::BeadFound(candidates)` (line 389)
   - Candidates are passed to PluckStrand, which filters them out
   - No work is done, strand never advances to workspace 2

4. **Why it's a deadlock**: The strand appears to have found work but all candidates are unclaimable. It returns to the worker loop, which re-queries the same workspaces in the same order, hitting the same deadlock on every iteration.

## The Fix

**Location**: After line 276 in `src/strand/explore.rs`

**Solution**: Add defensive label filtering following the PluckStrand pattern:

```rust
// Filter out assigned beads (belt-and-suspenders).
candidates.retain(|b| b.assignee.is_none());

// NEW: Filter out excluded labels (defensive).
candidates.retain(|b| !b.labels.iter().any(|l| filters.exclude_labels.contains(l)));
```

This ensures that even if the backend store doesn't filter by labels, the explore strand only returns candidates that can actually be claimed.

## Test Coverage

The file already includes comprehensive tests for this scenario (lines 716-921):

1. **`test_deadlock_multi_workspace_with_excluded_first_workspace`** (lines 716-781): Tests the exact deadlock scenario
2. **`deadlock_scenario_assigned_beads_allow_advancement`** (lines 800-863): Tests advancement when beads are assigned
3. **`deadlock_scenario_excluded_beads_allow_advancement`** (lines 870-921): Tests advancement when beads have excluded labels

These tests currently FAIL due to this bug. The fix will make them pass.

## Acceptance Criteria Met

- ✅ Current workspace iteration logic understood and documented
- ✅ Root cause of deadlock identified (missing defensive label filtering)
- ✅ Specific code location pinpointed (line 276 area in explore.rs)
- ✅ Fix location specified (add label filtering after assignee filtering)
- ✅ Test coverage documented (existing tests will verify fix)

## Related Code

- **Explore strand**: `/home/coding/NEEDLE/src/strand/explore.rs` lines 273-390
- **Pluck strand pattern**: `/home/coding/NEEDLE/src/strand/pluck.rs` lines 206-221
- **Test mocks**: `/home/coding/NEEDLE/src/strand/explore.rs` lines 1064-1477
