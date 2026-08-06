# BeadStatus::Deferred Implementation Verification (Bead bf-21qj5)

## Task
Add BeadStatus::Deferred so bf 'deferred' status stops failing deserialization (GitHub #10)

## Issue Description
`bf list --json` emits `status:deferred` but `BeadStatus` had no such variant, causing deserialization failures. This made 31 beads invisible to strands and supervise in the reporter's store (93 parse-error lines).

## Implementation Status
**COMPLETE** - Previously implemented in commit e97e88a (2026-07-28)

### Implementation Details (src/types/mod.rs)

1. **Variant Definition** (lines 111-117):
   - `BeadStatus::Deferred` variant added with documentation
   - Mirrors the Done/Closed aliasing precedent
   - Documents distinction from Blocked

2. **is_done() Behavior** (line 774):
   - `Deferred.is_done()` returns `false`
   - Deferred beads are not considered finished

3. **Display Implementation** (line 134):
   - `BeadStatus::Deferred` displays as `"deferred"`
   - Exhaustive match arm added

4. **Test Coverage**:
   - `bead_status_deferred_deserialization`: Verifies JSON round-trip
   - `bead_status_deferred_distinct_from_blocked`: Ensures Deferred ≠ Blocked
   - `bead_status_is_done`: Confirms is_done() = false
   - `bead_status_display`: Verifies display output
   - `bead_status_serialization`: Confirms snake_case serialization

### Verification Results
All 7 BeadStatus tests pass:
```
test types::tests::bead_status_closed_deserialization ... ok
test types::tests::bead_status_completed_deserialization ... ok
test types::tests::bead_status_deferred_deserialization ... ok
test types::tests::bead_status_deferred_distinct_from_blocked ... ok
test types::tests::bead_status_display ... ok
test types::tests::bead_status_is_done ... ok
test types::tests::bead_status_serialization ... ok
```

### Impact
This fix ensures that beads with `status:deferred` from bead-forge (bf) deserialize correctly instead of failing silently, making them visible to all strands and the supervisor.

## Related Documentation
- ADR-009: External Adopter Hardening
- plan.md Phase 13.4

## Verification Date
2026-08-05
