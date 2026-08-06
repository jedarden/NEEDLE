# BeadStatus::Deferred Verification - bf-21qj5

## Task Requirements
Add BeadStatus::Deferred so bf 'deferred' status stops failing deserialization (GH #10)

## Implementation Status ✅

The `BeadStatus::Deferred` variant has already been implemented in `src/types/mod.rs`.

### 1. BeadStatus::Deferred variant exists
- Location: `src/types/mod.rs:111-117`
- Properly documented with doc comments explaining its purpose
- Distinguished from Blocked (deferred = deliberately postponed, blocked = unmet dependency)
- Doc comment: "bf (bead-forge) emits \"deferred\" for beads deliberately postponed rather than blocked by a dependency. Distinct from `Blocked`: a deferred bead has no unmet dependency, it was just set aside — see GitHub issue jedarden/NEEDLE#10."

### 2. Serde configuration
- Uses `#[serde(rename_all = "snake_case")]` on enum (line 95)
- Deferred variant deserializes from "deferred" JSON string
- No explicit alias needed - matches snake_case convention
- Mirrors Done/Closed precedent as specified in ADR-009

### 3. is_done() implementation
- Returns `false` for Deferred (line 774)
- Deferred beads are explicitly NOT done (correct semantics)

### 4. Display implementation
- Exhaustive match arm (line 134): `BeadStatus::Deferred => write!(f, "deferred")`
- Displays as "deferred" in lowercase

### 5. Test coverage
Comprehensive tests in `src/types/mod.rs` (lines 788-805):
- `bead_status_deferred_deserialization` - Verifies "deferred" JSON deserializes correctly
- `bead_status_deferred_distinct_from_blocked` - Ensures Deferred ≠ Blocked
- `bead_status_is_done` - Confirms `is_done()` returns `false` for Deferred
- `bead_status_display` - Validates Display output is "deferred"
- `bead_status_serialization` - Confirms round-trip serialization produces "deferred"

## Impact
This fix resolves the deserialization failure that caused 31 beads to be silently invisible to strands and `supervise` in the reporter's bead store (93 parse-error lines). Deferred beads now:
- Deserialize successfully from `bf list --json` output
- Remain distinct from `Blocked` status (different semantics)
- Display correctly as "deferred"
- Are treated as non-terminal (not done) by the worker FSM

## Compliance with ADR-009
Implementation follows the decision in ADR-009 External-Adopter Hardening:
- Mirrors the existing `Done`/`Closed` aliasing precedent
- Maintains exhaustive match arms (no wildcards)
- Preserves backward compatibility (only accepts previously-rejected values)
- Does not change handling of previously-deserializing statuses

## Verification Results
All 7 BeadStatus tests pass:
- bead_status_completed_deserialization ✅
- bead_status_closed_deserialization ✅  
- bead_status_deferred_deserialization ✅
- bead_status_deferred_distinct_from_blocked ✅
- bead_status_display ✅
- bead_status_is_done ✅
- bead_status_serialization ✅

## Implementation History
This was implemented in commit e97e88a "fix: harden gates/spawn/bead-status per external adopter (GH #7-#11)" which addressed all 5 GitHub issues from jarvis-laboratories including #10 (Deferred status).

## Conclusion
The bead's deliverables are complete. The implementation fixes the exact issue described in GitHub #10, with comprehensive test coverage and full compliance with project conventions. The BeadStatus::Deferred variant is fully implemented and tested, allowing bf (bead-forge) stores containing "deferred" beads to deserialize correctly without silently dropping those beads from strand/supervise visibility.
