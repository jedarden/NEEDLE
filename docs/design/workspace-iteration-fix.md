# Workspace Iteration Fix for Explore Strand

**Status:** Design
**Tracking:** bead bf-k17qj
**Related:** ADR-001 (Explore Strand Hardening)

## Problem Statement

### Current Deadlock Scenario

The explore strand currently suffers from a return-on-first-candidates deadlock that prevents workers from advancing through workspaces when earlier workspaces have candidates that are not claimable by the current worker.

**Deadlock flow:**
1. Worker evaluates ExploreStrand against workspace list [ws1, ws2, ws3, ...]
2. Strand queries ws1 → store returns candidates (e.g., 2 ready beads)
3. Strand filters out assigned beads → 2 candidates remain
4. **Strand returns immediately** with these 2 candidates
5. Worker applies its own exclusion filters (race-lost TTL, manual exclusions)
6. Both candidates are excluded → worker gets NoWork
7. Next cycle: strand starts at ws1 again → **deadlock**

**The issue:** Workspace 2, 3, etc. are never checked because ws1 always appears to have candidates, even though none are claimable by the current worker.

### Why This Happens

There are **three layers** of filtering:

1. **Store layer** (`Filters::exclude_labels`): Filters out beads with `deferred`, `human`, `blocked` labels
2. **Strand layer** (`explore.rs line 273`): Filters out beads with `assignee != None`
3. **Worker layer** (`strand/mod.rs lines 307-318`): Filters out beads in worker exclusion set (race-lost TTL, manual exclusions)

The explore strand returns after layer 2, but layer 3 is what determines if a bead is actually claimable by the current worker. When layer 3 filters out all candidates, the deadlock occurs.

### Evidence from Tests

The codebase includes comprehensive tests that demonstrate this bug:

- `test_deadlock_multi_workspace_with_excluded_first_workspace` (explore.rs:712)
- `deadlock_scenario_assigned_beads_allow_advancement` (explore.rs:787)
- `deadlock_scenario_excluded_beads_allow_advancement` (explore.rs:843)

These tests currently **fail** on the implementation because the strand does not advance past workspace 1 when all its candidates are excluded.

## Design Solution

### Core Approach: Claimable-Aware Filtering

The fix implements **claimable-aware filtering** as specified in ADR-001:

> "Pass worker exclusion state into the strand's `Filters` so scan advancement is driven by *claimable-by-me* candidates, eliminating the deadlock class."

### Architecture Changes

#### 1. Modify `Filters` Structure

Add `exclude_ids` field to support filtering by bead IDs:

```rust
// src/bead_store/mod.rs
pub struct Filters {
    pub assignee: Option<String>,           // Existing: filter by assignee
    pub exclude_labels: Vec<String>,         // Existing: exclude by labels
    pub exclude_ids: HashSet<BeadId>,        // NEW: exclude by bead IDs
}
```

#### 2. Modify `BeadStore::ready()` Implementation

Update `BrCliBeadStore::ready()` to pass `exclude_ids` to the `br ready` command:

```rust
// src/bead_store/br_cli_bead_store.rs
impl BrCliBeadStore {
    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>> {
        let mut cmd = Command::new("br");
        cmd.args(["ready", "--json", "--limit", "1000"]);

        // Existing label filtering
        for label in &filters.exclude_labels {
            cmd.args(["--exclude-label", label]);
        }

        // NEW: ID-based filtering
        for bead_id in &filters.exclude_ids {
            cmd.args(["--exclude-id", bead_id.as_ref()]);
        }

        // ... rest of implementation
    }
}
```

**Note:** This requires bead-forge to support `--exclude-id` flag. If bead-forge doesn't support this yet, we filter IDs in-memory after fetching candidates.

#### 3. Modify `ExploreStrand::evaluate()` Signature

Update the strand to accept worker exclusion state:

```rust
// src/strand/explore.rs
#[async_trait::async_trait]
impl super::Strand for ExploreStrand {
    // OLD: evaluate(&self, store: &dyn BeadStore) -> StrandResult
    // NEW: Accept exclusions parameter
    async fn evaluate(&self, store: &dyn BeadStore, exclusions: &HashSet<BeadId>) -> StrandResult {
        // ... implementation
    }
}
```

#### 4. Update Worker to Pass Exclusions

Modify the worker's selection logic to pass exclusions to ExploreStrand:

```rust
// src/worker/mod.rs
let candidate = self
    .strands
    .select_with_exclusions(self.store.as_ref(), &exclusions)  // NEW method
    .await?;
```

#### 5. Update Strand Runner

Modify `StrandRunner::select()` to pass exclusions through to strands:

```rust
// src/strand/mod.rs
pub async fn select(
    &self,
    store: &dyn BeadStore,
    exclusions: &HashSet<BeadId>,
) -> Result<SelectOutcome> {
    // ...
    for strand in &self.strands {
        let result = strand.evaluate(store, exclusions).await;  // Pass exclusions
        // ...
    }
}
```

### Implementation Strategy

#### Phase 1: Modify Core Types (Low Risk)
1. Add `exclude_ids: HashSet<BeadId>` to `Filters` struct
2. Update default implementation
3. Add unit tests for Filters with exclude_ids

#### Phase 2: Update BeadStore Layer (Medium Risk)
1. Modify `BrCliBeadStore::ready()` to accept and use `exclude_ids`
2. If bead-forge doesn't support `--exclude-id`, filter in-memory
3. Add integration tests for exclude_ids filtering

#### Phase 3: Update Strand Interface (Medium Risk)
1. Change `Strand::evaluate()` signature to accept `exclusions: &HashSet<BeadId>`
2. Update all strand implementations:
   - `PluckStrand::evaluate()` - ignore exclusions (home workspace only)
   - `ExploreStrand::evaluate()` - USE exclusions (core fix)
   - `MendStrand::evaluate()` - ignore exclusions (cleanup strand)
   - Other idle strands - ignore exclusions

#### Phase 4: Update Worker and StrandRunner (Low Risk)
1. Modify `StrandRunner::select()` to pass exclusions to strands
2. Ensure worker builds correct exclusion set before calling `select()`
3. Add telemetry for "candidates excluded by worker exclusions"

#### Phase 5: Tests and Verification (Critical)
1. Enable existing deadlock tests (should now pass)
2. Add new tests for race-lost exclusion filtering
3. Add integration test with 2 workers claiming from same workspace
4. Add telemetry verification test

## Edge Cases and Considerations

### Edge Case 1: Empty Exclusion Set
**Scenario:** Worker has no exclusions (first cycle, no race losses)
**Behavior:** Works exactly as before - no filtering by exclude_ids
**Risk:** None - backward compatible

### Edge Case 2: All Candidates Excluded
**Scenario:** Every workspace has candidates, but all are in worker's exclusion set
**Current Behavior:** Returns first workspace's candidates → worker filters them all out → NoWork
**Fixed Behavior:** Strand advances through all workspaces → returns NoWork at end
**Risk:** Low - correct behavior, just more store traffic

### Edge Case 3: Mixed Exclusions
**Scenario:** Workspace 1 has 3 candidates: 1 excluded, 2 not excluded
**Current Behavior:** Returns all 3 → worker filters out 1 → claims from remaining 2
**Fixed Behavior:** Returns only 2 (filter applied at strand level)
**Risk:** None - behavioral improvement, no functional change

### Edge Case 4: Exclusion TTL Expiration
**Scenario:** Worker has race-lost exclusions with 30s TTL
**Current Behavior:** Strand doesn't know about exclusions, returns excluded beads
**Fixed Behavior:** Worker passes current exclusions (including expired ones) to strand
**Consideration:** Strand filters by current exclusion state, which may include expired entries
**Resolution:** This is correct - exclusions should be pruned at worker level before passing to strand

### Edge Case 5: bead-forge Doesn't Support --exclude-id
**Scenario:** Deployed bead-forge version lacks `--exclude-id` flag
**Behavior:** Filter IDs in-memory after `br ready` fetches all candidates
**Performance:** Slightly more network traffic, but functionally correct
**Risk:** Low - defensive fallback already exists for label filtering

### Edge Case 6: Concurrent Exclusion Updates
**Scenario:** Worker updates exclusions during strand evaluation
**Current Behavior:** Strand reads exclusions once at start of evaluate()
**Fixed Behavior:** Same - exclusions parameter is immutable for duration of call
**Risk:** None - exclusions are passed by value, not reference

### Edge Case 7: Multi-Worker Race Conditions
**Scenario:** 10 workers all evaluating ExploreStrand against same workspace
**Current Behavior:** All workers see same candidates, race for same bead, 9 lose
**Fixed Behavior:** Workers with exclusions skip already-raced beads (ADR-001, item 2: per-worker scan rotation)
**Risk:** None - this is the intended fix for thundering herd

## Preserving Existing Functionality

### What Doesn't Change

1. **Static workspace list:** Still captured at boot, not re-discovered mid-flight
2. **Workspace iteration order:** Still sequential (scan rotation is separate, ADR-001 item 2)
3. **Label filtering:** Still uses `exclude_labels` for deferred/human/blocked
4. **Assignee filtering:** Still filters `assignee != None` at strand level
5. **Home workspace skipping:** Still skips home workspace
6. **Cross-workspace mend:** Still runs when no candidates found
7. **Return-on-first-valid:** Still returns immediately when claimable candidates found

### What Changes

1. **Strand evaluates "claimable-by-me" not just "ready"**
2. **Worker exclusions applied at strand level, not post-selection**
3. **Deadlock broken: strand advances past workspaces with no claimable candidates**

## Testing Strategy

### Unit Tests

1. **Filters with exclude_ids**
   ```rust
   #[test]
   fn filters_with_exclude_ids_filters_correctly() {
       let filters = Filters {
           exclude_ids: [BeadId::from("excluded"), BeadId::from("also-excluded")]
               .into_iter()
               .collect(),
           // ... other fields
       };
       // Test filtering logic
   }
   ```

2. **BeadStore respects exclude_ids**
   ```rust
   #[tokio::test]
   async fn ready_filters_excluded_ids() {
       let store = setup_test_store();
       let filters = Filters {
           exclude_ids: [BeadId::from("bead-1")].into_iter().collect(),
           // ...
       };
       let candidates = store.ready(&filters).await;
       assert!(!candidates.iter().any(|b| b.id == "bead-1"));
   }
   ```

### Integration Tests

1. **Deadlock scenario tests now pass**
   - `test_deadlock_multi_workspace_with_excluded_first_workspace`
   - `deadlock_scenario_assigned_beads_allow_advancement`
   - `deadlock_scenario_excluded_beads_allow_advancement`

2. **Multi-worker race test**
   ```rust
   #[tokio::test]
   async fn two_workers_dont_race_on_same_bead() {
       // Setup: workspace with 1 ready bead
       // Act: spawn 2 workers concurrently
       // Assert: exactly 1 worker claims, 1 returns NoWork
   }
   ```

3. **TTL expiration test**
   ```rust
   #[tokio::test]
   async fn expired_exclusions_not_propagated_to_strand() {
       // Setup: worker with expired race-lost exclusion
       // Act: call strand.select() with exclusions
       // Assert: expired ID not in filters passed to store
   }
   ```

### Manual Testing

1. **Lab deployment verification**
   - Deploy to 4-worker lab fleet
   - Create 24 ready beads across 24 workspaces
   - Monitor: all beads should be claimed within ~30 minutes (not 40 hours)

2. **Telemetry verification**
   - Check `strand.evaluated` events have correct `candidates` and `excluded` counts
   - Verify `strand.find_explore_starvation` emitted when appropriate

## Telemetry Additions

### New Events

1. **Strand exclusion filtering**
   ```json
   {
     "kind": "strand.exclusion_filtered",
     "strand_name": "explore",
     "workspace": "/path/to/ws",
     "candidates_before": 10,
     "candidates_after": 7,
     "excluded_by_labels": 2,
     "excluded_by_ids": 1
   }
   ```

2. **Workspace advancement**
   ```json
   {
     "kind": "strand.workspace_advanced",
     "strand_name": "explore",
     "from_workspace": "/path/to/ws1",
     "to_workspace": "/path/to/ws2",
     "reason": "all_candidates_excluded"
   }
   ```

### Updated Events

1. **Strand evaluated** (existing, add fields)
   ```json
   {
     "kind": "strand.evaluated",
     "strand_name": "explore",
     "result": "bead_found",
     "duration_ms": 45,
     "candidates": 5,
     "excluded": 3,           // NEW: count of excluded candidates
     "exclusion_reason": {    // NEW: breakdown
       "labels": 1,
       "assignee": 1,
       "worker_ids": 1
     }
   }
   ```

## Implementation Phases

### Phase 1: Core Types (Day 1)
- [ ] Add `exclude_ids` to `Filters` struct
- [ ] Update `Filters::default()`
- [ ] Add unit tests

### Phase 2: BeadStore Layer (Day 1-2)
- [ ] Modify `BrCliBeadStore::ready()` to use `exclude_ids`
- [ ] Implement in-memory fallback if bead-forge lacks `--exclude-id`
- [ ] Add integration tests

### Phase 3: Strand Interface (Day 2-3)
- [ ] Update `Strand` trait `evaluate()` signature
- [ ] Update `PluckStrand::evaluate()` (ignore exclusions)
- [ ] Update `ExploreStrand::evaluate()` (USE exclusions)
- [ ] Update `MendStrand::evaluate()` (ignore exclusions)
- [ ] Update all other idle strands (ignore exclusions)

### Phase 4: Worker Integration (Day 3-4)
- [ ] Modify `StrandRunner::select()` to pass exclusions
- [ ] Update worker to build correct exclusion set
- [ ] Add new telemetry events
- [ ] Update existing telemetry events

### Phase 5: Testing and Verification (Day 4-5)
- [ ] Enable existing deadlock tests
- [ ] Add new unit tests
- [ ] Add integration tests
- [ ] Run full test suite
- [ ] Manual deployment testing

### Phase 6: Documentation (Day 5)
- [ ] Update this design with implementation notes
- [ ] Update ADR-001 status to "Implemented"
- [ ] Add migration notes if needed

## Rollout Plan

### Staged Deployment

1. **Dev cluster:** Deploy to single-worker dev instance
   - Verify basic functionality
   - Check telemetry emission

2. **Lab cluster:** Deploy to 4-worker lab fleet
   - Test multi-worker scenarios
   - Verify deadlock resolution

3. **Production:** Deploy to production fleet
   - Monitor for 24 hours
   - Check throughput metrics

### Rollback Criteria

Rollback if any of:
- New tests fail in CI
- Worker crash rate increases > 10%
- Throughput decreases > 20%
- Telemetry shows unexpected exclusion patterns

## Success Metrics

### Functional
- [ ] All 3 deadlock tests pass
- [ ] Lab fleet processes 24 beads across 24 workspaces in < 60 minutes
- [ ] No regression in existing strand tests

### Performance
- [ ] No significant increase in store traffic (< 10%)
- [ ] Worker selection latency unchanged (< 5% variance)

### Reliability
- [ ] No worker crashes related to exclusion filtering
- [ ] Telemetry correctly reports exclusion counts
- [ ] Manual exclusions still work (test with `br block`)

## Open Questions

1. **bead-forge version compatibility:** Does deployed bead-forge support `--exclude-id`?
   - **Resolution:** Implement in-memory fallback, check bead-forge version

2. **Strand interface change:** Is updating all `Strand::evaluate()` signatures safe?
   - **Resolution:** Yes, trait change is additive (all implementors are in this repo)

3. **Exclusion set size:** What if exclusion set has 1000+ IDs?
   - **Resolution:** Unlikely (TTL is 30s, max exclusions bounded by concurrent workers)

## References

- ADR-001: Explore Strand Hardening
- bead bf-1d64q: Initial deadlock investigation
- bead bf-k17qj: This design task
- src/strand/explore.rs: Explore strand implementation
- src/strand/mod.rs: Strand runner and waterfall
- src/worker/mod.rs: Worker exclusion state management
