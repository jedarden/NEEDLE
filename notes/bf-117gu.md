# Explore Strand Deadlock Summary

## Deadlock Scenario

### The Issue
The `ExploreStrand::evaluate()` method (src/strand/explore.rs) iterates through workspaces in a fixed order and returns at the first workspace with "ready" candidates. However, **worker-level filtering happens AFTER the strand returns**.

### The Deadlock
If a workspace has candidates but they are all:
1. **Excluded** - Already attempted and failed within the race-lost TTL (30s)
2. **Already assigned** - Assigned to other workers

Then:
1. `ExploreStrand::evaluate()` returns `StrandResult::BeadFound(candidates)` 
2. The claiming code filters by exclusions and assignees
3. Zero beads remain claimable
4. Worker enters idle backoff (observed at **900s**)
5. Loop repeats, never advancing to workspace #2

### Real-World Impact
**2026-07-11 lab incident:**
- 24 ready beads across 24 workspaces (one per workspace)
- 4 roaming workers available
- Throughput: **~1 bead per 40 minutes**
- Same pinned workers processed **hundreds of beads** in the same period

### Root Causes (from ADR-001)

1. **Return-on-first-candidates deadlock** - `evaluate()` returns at first workspace with ready candidates, but unclaimable filtering happens later
2. **Thundering herd** - All workers walk the same list in the same order, all converge on the same store
3. **Store-layer limit bugs** - No `--limit` passed (truncates output) or `--limit 0` (returns empty)
4. **Stale assignees are permanent** - `reopen` doesn't clear assignee; cross-workspace mend only handles orphans
5. **Claim errors masquerade as races** - CLI failures collapse into `claimed_by=(race)`
6. **Boot-only discovery** - New stores require worker restarts

## Proposed Fix Approach (Phase 5 Plan)

### 5.1 Selection Correctness

**Claimable-aware candidate filtering:**
- Pass worker exclusion state into the strand's `Filters`
- Scan advancement driven by **claimable-by-me** candidates, not just "ready" candidates
- Loop advances past workspaces with nothing claimable by this worker

**Per-worker scan rotation:**
- Start iteration at `hash(qualified_id) % N`, wrapping around
- Workers partition the workspace list instead of racing for the same first store

**Store-layer limit correctness:**
- Always pass explicit large limit to `br ready --json`
- Add boot-time `bf --version` handshake that WARNs on known-bad versions

### 5.2 Stale-State Healing

**Mend releases stale assignees on open beads:**
- Extend cross-workspace mend to handle open beads with dead assignees
- Clear assignees when the assigned worker has no live heartbeat

**Claim-error taxonomy:**
- Distinguish error from race in claim results
- After N consecutive errors, emit ERROR telemetry and mark bead/store suspect

### 5.3 Cadence and Liveness

**Event-driven wakeups + jittered floor:**
- Replace flat idle backoff (900s) with mtime/inotify watches on `.beads/issues.jsonl`
- Jittered 60-120s polling floor
- Found-but-excluded triggers short retry, never idle backoff

**Periodic re-discovery:**
- Re-run workspace discovery every N cycles
- No upward traversal constraint remains

### 5.4 Observability

**Per-cycle scan telemetry:**
- Workspaces visited
- Candidates found
- Exclusion reasons

**Starvation alarm:**
- Ready beads exist but worker claimed nothing for X minutes
- Surface last-scan-per-workspace in `needle status`

## Code Evidence

From `src/strand/explore.rs` (current implementation):

```rust
// Line 156-163: Filters don't include worker exclusions
let filters = Filters {
    assignee: None,  // ← Only filters by assignee=None
    exclude_labels: vec![
        "deferred".to_string(),
        "human".to_string(),
        "blocked".to_string(),
    ],
};

// Line 191-194: Query happens BEFORE worker-level filtering
match remote_store.ready(&filters).await {
    Ok(mut candidates) => {
        candidates.retain(|b| b.assignee.is_none());  // ← Too late!
```

The strand returns candidates based on store-layer filters, but the worker's exclusion set (race-lost beads) is never passed down. This creates the deadlock: a workspace with ready-but-unclaimable beads satisfies `ready()` but produces zero claims.

## Exit Criteria

- 24 stores scenario with 4 workers drains in **minutes**, not hours
- Beads flushed to any store claimed without worker restart
- `needle status` shows "when did each workspace last get scanned, and why was nothing claimed"
