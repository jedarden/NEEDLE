# bf-1hm79: Per-Worker Scan-Order Rotation (De-Herding) - Implementation Summary

## Task
Part of plan.md Phase 5 and ADR-001 (explore strand hardening).

**Scope:** All workers iterate the static workspace list in the same order and converge on the same store. Start iteration at `hash(qualified_id) % list_length` and wrap around, so workers partition the estate. Keep sort determinism within a workspace.

**Acceptance:** Unit test that two workers with different qualified ids visit workspaces in different rotations covering the full list.

## Implementation Status: ✓ COMPLETE

The per-worker scan-order rotation feature is fully implemented in `src/strand/explore.rs` (commit 4d42244).

### Core Implementation

1. **`compute_start_index()`** (lines 247-261)
   - Computes starting position using `hash(qualified_id) % workspace_count`
   - Uses stable `DefaultHasher` for deterministic hashing
   - Returns 0 for empty workspace list (defensive)

2. **`rotated_workspace_order()`** (lines 263-288)
   - Returns workspaces in rotation order starting at computed index
   - Wraps around to cover all workspaces exactly once
   - Each worker with different qualified_id gets different rotation

3. **`evaluate()` Integration** (lines 347-357)
   - Uses `rotated_workspace_order()` instead of static list
   - Logs start index and qualified_id for debugging
   - Maintains sort determinism within each workspace (priority ASC, created_at ASC, id ASC)

### Unit Tests (All Passing)

- `rotation_start_index_is_deterministic_for_same_qualified_id` - Same ID produces same start
- `rotation_start_index_differs_for_different_qualified_ids` - Different IDs produce different starts
- `rotated_workspace_order_covers_all_workspaces` - All workspaces visited exactly once
- `rotation_starts_at_computed_index` - Rotation starts at correct position
- `two_workers_with_different_ids_have_different_rotations` - De-herding verified
- `rotation_with_single_workspace_returns_same_order` - Edge case handling
- `rotation_with_empty_workspaces_returns_empty` - Empty list handling
- `rotation_hash_distribution_is_reasonable` - 50 workers across 10 workspaces distributed well

### Code Quality

- ✓ No `unwrap()` or `expect()` in non-test code
- ✓ Clippy clean (no warnings in explore.rs)
- ✓ Formatted with `cargo fmt`
- ✓ Exhaustive match arms
- ✓ Telemetry events emitted at state transitions

### CI Workflow

- Implementation already pushed to main (commit 4d42244)
- CI workflow: `needle-ci`
- Commit is integrated into main (newer commits exist after it)

## How It Works

Before (herding problem):
```
Worker A: [ws1, ws2, ws3, ws4]  → all workers start at ws1, race for same beads
Worker B: [ws1, ws2, ws3, ws4]
Worker C: [ws1, ws2, ws3, ws4]
```

After (de-herded rotation):
```
Worker A (hash%4 = 0): [ws1, ws2, ws3, ws4]
Worker B (hash%4 = 2): [ws3, ws4, ws1, ws2]
Worker C (hash%4 = 1): [ws2, ws3, ws4, ws1]
```

Each worker starts at a different position and scans all workspaces in a different order, naturally partitioning the workload across the fleet.

## References

- ADR-001: `docs/adr/001-explore-strand-hardening.md`
- Plan Phase 5.1: `docs/plan/plan.md#51-selection-correctness`
- Implementation: `src/strand/explore.rs` lines 240-288, 347-357
- Tests: `src/strand/explore.rs` lines 834-1132

## Verified By

Claude Code (glm-4.7) on 2026-07-21
- All rotation tests pass
- Code quality checks pass
- Implementation matches ADR-001 and Phase 5.1 requirements
