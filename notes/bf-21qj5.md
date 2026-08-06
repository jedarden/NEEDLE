# Bead bf-21qj5: Add BeadStatus::Deferred - COMPLETED

## Task Summary
Add `BeadStatus::Deferred` variant so `bf list --json` output with `status:deferred` stops failing deserialization (GitHub issue #10).

## Implementation Status: ✅ COMPLETE

The `BeadStatus::Deferred` variant has already been implemented in `src/types/mod.rs`:

### Code Changes (Already Implemented)

**1. BeadStatus Enum (lines 111-117)**
```rust
/// `bf` (bead-forge) emits `"deferred"` for beads deliberately postponed
/// rather than blocked by a dependency. Distinct from `Blocked`: a
/// deferred bead has no unmet dependency, it was just set aside — see
/// GitHub issue jedarden/NEEDLE#10. Without this variant, `bf list --json`
/// fails deserialization for every such bead, and it silently disappears
/// from strand/supervise visibility with no surfaced error.
Deferred,
```

**2. is_done() Method (line 774)**
- Returns `false` for `Deferred` status (deferred beads are not done)

**3. Display Implementation (line 134)**
- Returns `"deferred"` string representation

**4. Comprehensive Test Coverage (lines 788-805)**
- `bead_status_deferred_deserialization`: Verifies "deferred" JSON deserializes correctly
- `bead_status_deferred_distinct_from_blocked`: Ensures Deferred ≠ Blocked
- `bead_status_is_done`: Confirms `is_done()` returns `false` for Deferred
- `bead_status_display`: Validates Display output is "deferred"
- `bead_status_serialization`: Confirms round-trip serialization produces "deferred"

### Impact
This fix resolves the deserialization failure that caused 31 beads to be silently invisible to strands and `supervise` in the reporter's bead store (93 parse-error lines). Deferred beads now:
- Deserialize successfully from `bf list --json` output
- Remain distinct from `Blocked` status (different semantics)
- Display correctly as "deferred"
- Are treated as non-terminal (not done) by the worker FSM

### Compliance with ADR-009
Implementation follows the decision in ADR-009 External-Adopter Hardening:
- Mirrors the existing `Done`/`Closed` aliasing precedent
- Maintains exhaustive match arms (no wildcards)
- Preserves backward compatibility (only accepts previously-rejected values)
- Does not change handling of previously-deserializing statuses

### Test Results
All 7 BeadStatus tests pass:
- ✅ bead_status_serialization
- ✅ bead_status_is_done
- ✅ bead_status_display
- ✅ bead_status_deferred_deserialization
- ✅ bead_status_deferred_distinct_from_blocked
- ✅ bead_status_closed_deserialization
- ✅ bead_status_completed_deserialization

## Conclusion
The bead's deliverables are complete. The implementation fixes the exact issue described in GitHub #10, with comprehensive test coverage and full compliance with project conventions.
