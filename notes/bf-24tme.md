# Explore Strand Deadlock Analysis

## Task: Analyze explore strand implementation and identify deadlock condition

Date: 2026-07-14  
Bead: bf-24tme  
Status: Complete

---

## Overview

The explore strand in `src/strand/explore.rs` implements multi-workspace bead discovery for the NEEDLE worker. This analysis documents how candidate filtering works, identifies the specific deadlock condition, and explains what the test needs to prove.

---

## How Candidates are Filtered and Selected Across Workspaces

### Two-Stage Filtering Architecture

The explore strand uses a two-stage filtering approach:

#### Stage 1: Database-Level Filtering (via `br ready`)

```rust
// src/strand/explore.rs:235-242
let filters = Filters {
    assignee: None,  // No assignee filtering at DB level
    exclude_labels: vec![
        "deferred".to_string(),
        "human".to_string(),
        "blocked".to_string(),
    ],
};
```

**What this does:**
- Queries `br ready --json` with label exclusions
- Returns all `status: Open` beads EXCEPT those with excluded labels
- **Important:** Does NOT filter by assignee at the database level
- Result: May include beads with `assignee: Some("worker-xyz")`

#### Stage 2: Client-Side Filtering (Belt-and-Suspenders)

```rust
// src/strand/explore.rs:270-273
match remote_store.ready(&filters).await {
    Ok(mut candidates) => {
        // Filter out assigned beads (belt-and-suspenders).
        candidates.retain(|b| b.assignee.is_none());
```

**What this does:**
- Removes any beads where `assignee.is_some()` (assigned to another worker)
- This is "belt-and-suspenders" because the DB should already exclude assigned beads
- Defensive programming: protects against race conditions or inconsistent state

### Filtering Sequence Summary

```
DB Query → [Open beads, no excluded labels] → Client Filter → [Unassigned only]
```

**Key Insight:** The "belt-and-suspenders" comment (line 272) indicates this is a defensive check. The `br ready` command SHOULD already filter out assigned beads, but we do it again in-memory to be absolutely certain.

---

## What 'Excluded' and 'Assigned' Mean in Filtering Logic

### 'Excluded' Beads

Beads excluded by the **label filter** at the database level:
- **deferred**: Beads marked for later processing
- **human**: Beads requiring human intervention
- **blocked**: Beads blocked by dependencies

These beads are excluded BEFORE they leave the database:

```rust
// src/bead_store/mod.rs:593-595 (BrCliBeadStore::ready)
if !filters.exclude_labels.is_empty() {
    beads.retain(|b| !b.labels.iter().any(|l| filters.exclude_labels.contains(l)));
}
```

### 'Assigned' Beads

Beads with an active assignee (being worked on by another worker):
- **assignee: Some("worker-abc")**: Currently claimed by a worker
- **assignee: None**: Available for claiming

These are filtered AFTER retrieval, in-memory:

```rust
// src/strand/explore.rs:273
candidates.retain(|b| b.assignee.is_none());
```

---

## Current Workspace Iteration Order and Logic

### Static Workspace List (Lines 49-50)

```rust
/// Static list of workspace paths to search (in order).
workspaces: Vec<PathBuf>,
```

**Key characteristics:**
- Captured at construction time from config
- Never re-read during runtime
- Order determined by configuration (explicit list or discovery order)

### Iteration Logic (Lines 244-397)

```rust
for workspace in &self.workspaces {  // Line 244
    // 1. Skip home workspace (line 246-249)
    if workspace == &self.home_workspace {
        continue;
    }

    // 2. Skip workspaces without .beads/ (line 252-255)
    if !Self::has_beads_dir(workspace) {
        continue;
    }

    // 3. Query for candidates (line 258-268)
    let remote_store = match self.store_factory.create_store(workspace).await {
        Ok(s) => s,
        Err(e) => { continue; }  // Skip on error
    };

    // 4. Filter candidates (lines 270-273)
    match remote_store.ready(&filters).await {
        Ok(mut candidates) => {
            candidates.retain(|b| b.assignee.is_none());

            // 5. If empty, try cross-workspace mend (lines 275-363)
            if candidates.is_empty() {
                // ... run mend, re-query, filter again ...
                continue;  // Line 363: Advance to next workspace
            }

            // 6. Found candidates, return them (lines 366-386)
            return StrandResult::BeadFound(candidates);
        }
        Err(e) => {
            continue;  // Skip on query error
        }
    }
}
```

### Iteration Guarantees

**The strand ALWAYS advances to the next workspace when:**
- Current workspace has no valid candidates after filtering (line 363)
- Query fails for any reason
- Workspace has no `.beads/` directory
- Workspace is the home workspace

**The strand STOPS iteration when:**
- Candidates are found in any workspace (line 386)
- All workspaces have been exhausted (line 399: returns `NoWork`)

---

## The Deadlock Condition: Detailed Analysis

### What the Test Describes (Lines 704-710)

```rust
/// DEADLOCK SCENARIO (from bf-1d64q):
/// 1. Workspace 1 has candidates but all are assigned or excluded
/// 2. Workspace 2 has valid unassigned candidates
/// 3. EXPECTED: Strand advances past workspace 1 to workspace 2
/// 4. BUG: Strand returns NoWork prematurely, never checking workspace 2
```

### Understanding the Terminology

The test comment says "assigned or excluded", but looking at the filtering logic:
- **"Excluded"** here refers to label-based exclusion (deferred/human/blocked)
- **"Assigned"** refers to having an assignee set

However, there's a critical distinction:
- **Label-excluded beads** are filtered at the DB level (never returned by `br ready`)
- **Assigned beads** might be returned by `br ready` (if the DB doesn't filter them), then filtered in-memory

### The Scenario Step-by-Step

**Given:**
- Workspace 1: `/tmp/test/workspace1`
- Workspace 2: `/tmp/test/workspace2`
- Home: `/home/test`

**Iteration Flow:**

1. **Process Workspace 1:**
   ```
   remote_store.ready(&filters).await
   → Returns: [bead1(assignee="worker-A"), bead2(assignee="worker-B")]
   
   candidates.retain(|b| b.assignee.is_none())
   → candidates is now empty (all were assigned)
   ```

2. **Enter Mend Logic (lines 275-363):**
   ```rust
   if candidates.is_empty() {
       // Run cross-workspace mend to release orphans
       match cleanup_orphaned_in_progress(...) {
           Ok(released) => {
               // In this scenario: released = 0
               // (assigned beads != orphaned in-progress beads)
               
               // Re-query after cleanup
               remote_store.ready(&filters).await
               → Still returns [bead1, bead2] (still assigned)
               
               retry_candidates.retain(|b| b.assignee.is_none())
               → retry_candidates is still empty
               
               // Line 363: continue to next workspace
               continue;
           }
       }
   }
   ```

3. **Process Workspace 2:**
   ```rust
   remote_store.ready(&filters).await
   → Returns: [bead3(assignee=None)]  // Valid candidate!
   
   candidates.retain(|b| b.assignee.is_none())
   → candidates = [bead3]  // Passes filter
   
   // Line 386: Return candidates
   return StrandResult::BeadFound(candidates);
   ```

### The Current Implementation is CORRECT

Looking at the code flow, the **current implementation handles this correctly**:
- Line 363: `continue` advances to the next workspace
- The strand DOES check workspace 2
- The strand DOES return workspace 2's candidates

### So Where Was the Bug?

The test comment says "BUG: Strand returns NoWork prematurely", but the current code doesn't have this bug. Looking at the test infrastructure:

**Test Setup (lines 719-738):**
```rust
let mock_factory = Arc::new(DeadlockMockStoreFactory::new(...));
let strand = ExploreStrand::new_with_store_factory(..., mock_factory.clone(), ...);
let result = strand.evaluate(&store).await;
```

The test uses **injected store factories** to simulate specific scenarios. This suggests:
1. The test was written to prove a fix for a past bug
2. The test ensures the bug doesn't regress
3. The current code passes the test (bug is fixed)

### Historical Context

The bead ID `bf-1d64q` mentioned in the test comment suggests this was a real bug that was fixed. The test documents:
- **What the bug was:** Strand stopped iteration when one workspace had no valid candidates
- **What the fix is:** Always `continue` to next workspace when candidates are empty
- **How to prove it:** This test verifies workspace 2 is checked after workspace 1 is empty

---

## Code Location Where Deadlock Occurred (Historical)

**If the bug were to exist, it would be at line 363:**

```rust
// src/strand/explore.rs:363
// Advance to next workspace (candidates empty after mend).
continue;
```

**If this were `return StrandResult::NoWork;` instead of `continue;`, the bug would manifest.**

The current implementation uses `continue;` which is correct.

---

## What the Test Needs to Prove

### Test 1: Assigned Beads Allow Advancement (Lines 718-768)

**Test:** `deadlock_scenario_assigned_beads_allow_advancement`

**Setup:**
- Workspace 1: Returns 2 candidates, both with assignees
- Workspace 2: Returns 1 unassigned candidate

**Must Prove:**
1. Both workspaces are queried (line 745: `call_count >= 2`)
2. Workspace 2's candidate IS returned (lines 748-753)
3. The strand does NOT return `NoWork` prematurely (line 756)

**Mock Implementation:**
```rust
// Lines 905-1015: AssignedBeadsStore
async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
    // First query: return assigned beads (filtered out by strand)
    Ok(vec![
        Bead { assignee: Some("other-worker-1"), ... },
        Bead { assignee: Some("other-worker-2"), ... },
    ])
}
```

### Test 2: Excluded Beads Allow Advancement (Lines 774-819)

**Test:** `deadlock_scenario_excluded_beads_allow_advancement`

**Setup:**
- Workspace 1: Returns beads with "blocked" label (excluded by filters)
- Workspace 2: Returns 1 valid unassigned candidate

**Must Prove:**
1. Workspace 2's candidate IS returned (lines 800-804)
2. The strand does NOT stop at workspace 1 (line 807)

**Mock Implementation:**
```rust
// Lines 1018-1113: BlockedBeadsStore
async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
    // Return beads with "blocked" label (excluded by filters)
    Ok(vec![
        Bead { labels: vec!["blocked".to_string()], ... }
    ])
}
```

---

## Summary

### Key Findings

1. **Candidate Filtering:**
   - Two-stage process: DB-level (labels) + client-side (assignee)
   - "Belt-and-suspenders" approach for robustness

2. **'Excluded' vs 'Assigned':**
   - Excluded: Label-based filtering at DB level
   - Assigned: Client-side filtering after retrieval

3. **Workspace Iteration:**
   - Static order from config
   - Always advances when current workspace has no valid candidates
   - Only stops when candidates found or all workspaces exhausted

4. **Deadlock Condition:**
   - Historical bug where strand stopped at first empty workspace
   - Current implementation is correct (uses `continue` at line 363)
   - Tests ensure this doesn't regress

### What the Test Proves

Both tests verify that **workspace iteration continues even when one workspace has no valid candidates**, ensuring the strand doesn't deadlock at the first empty workspace.

---

## References

- **explore.rs implementation:** `src/strand/explore.rs` (lines 212-407)
- **BeadStore trait:** `src/bead_store/mod.rs` (lines 96-187)
- **Bead type definition:** `src/types/mod.rs` (lines 371-398)
- **Test infrastructure:** `src/strand/explore.rs` (lines 700-1198)

---

**End of Analysis**
