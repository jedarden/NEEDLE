# Workspace Iteration Fix - Design Summary

**Task:** bf-k17qj - Design workspace iteration fix for explore strand
**Status:** Complete - Design documented
**Date:** 2026-07-14

## What Was Done

Created comprehensive design document at `docs/design/workspace-iteration-fix.md` covering:

### Problem Analysis
- Documented the return-on-first-candidates deadlock
- Explained the three-layer filtering architecture
- Identified where the strand returns too early
- Linked to existing failing tests

### Solution Design
- **Core approach:** Claimable-aware filtering (from ADR-001)
- Pass worker exclusion state into strand's Filters
- Apply exclusions at strand level, not post-selection
- Strand advances when workspace has no claimable-by-me candidates

### Implementation Plan
1. Modify `Filters` struct to add `exclude_ids: HashSet<BeadId>`
2. Update `BrCliBeadStore::ready()` to filter by excluded IDs
3. Change `Strand::evaluate()` signature to accept exclusions parameter
4. Update worker/StrandRunner to pass exclusions to strands
5. Enable existing deadlock tests (they should now pass)

### Edge Cases Covered
- Empty exclusion set (backward compatible)
- All candidates excluded (correct NoWork behavior)
- Mixed exclusions (behavioral improvement)
- Exclusion TTL expiration (correct pruning)
- bead-forge without --exclude-id support (in-memory fallback)
- Multi-worker race conditions (intended thundering herd fix)

### Testing Strategy
- Unit tests for Filters with exclude_ids
- Integration tests for BeadStore filtering
- Enable 3 existing deadlock scenario tests
- Add multi-worker race test
- Manual lab deployment verification

### Phased Implementation
- Phase 1: Core types (Filters struct)
- Phase 2: BeadStore layer
- Phase 3: Strand interface updates
- Phase 4: Worker integration
- Phase 5: Testing and verification
- Phase 6: Documentation

## Key Design Decisions

1. **Modify Filters struct** rather than passing exclusions separately
   - Cleaner API, all filtering in one place
   - Matches existing pattern with exclude_labels

2. **In-memory fallback** if bead-forge lacks --exclude-id
   - Defensive programming, handles version skew
   - Slightly more network traffic but functionally correct

3. **All strands get exclusions parameter** even if they ignore it
   - Cleaner trait interface
   - ExploreStrand is the only one that uses it (others ignore)

4. **Exclusions passed by value** (HashSet is cloneable)
   - Simpler than lifetime management
   - Small data structure (typically < 10 entries)

## Success Criteria

When implemented:
- [ ] All 3 deadlock tests pass (explore.rs:712, 787, 843)
- [ ] Lab fleet processes 24 beads across 24 workspaces in < 60 minutes
- [ ] No regression in existing tests
- [ ] Telemetry correctly reports exclusion counts

## Next Steps

The design is complete. The next bead should:
1. Implement Phase 1 (Core types)
2. Implement Phase 2 (BeadStore layer)
3. Implement Phase 3-4 (Strand interface)
4. Verify all tests pass
5. Deploy and test in lab environment

## References

- Design document: `docs/design/workspace-iteration-fix.md`
- ADR-001: `docs/adr/001-explore-strand-hardening.md`
- Existing tests: `src/strand/explore.rs` lines 712-887
