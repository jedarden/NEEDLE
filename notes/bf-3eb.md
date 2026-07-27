# bf-3eb: exclude_ids Feature Already Implemented

## Summary
The `exclude_ids` feature for the Filters struct was already fully implemented in the codebase. This note documents the verification that all acceptance criteria were already met.

## Implementation Details

### Filters Struct (src/bead_store/mod.rs:251-268)
- ✅ `exclude_ids: HashSet<BeadId>` field present (line 257)
- ✅ Default implementation initializes empty HashSet (line 265)
- ✅ Backward compatible (empty set = no-op)

### BrCliBeadStore::ready() (src/bead_store/mod.rs:1039-1041)
```rust
// Apply ID exclusion filter (in-memory filter).
if !filters.exclude_ids.is_empty() {
    beads.retain(|b| !filters.exclude_ids.contains(&b.id));
}
```

### BfCliBeadStore::ready() (src/bead_store/mod.rs:1712-1714)
```rust
// Apply ID exclusion filter (in-memory filter).
if !filters.exclude_ids.is_empty() {
    beads.retain(|b| !filters.exclude_ids.contains(&b.id));
}
```

## Test Coverage
All unit tests pass (41/41 bead_store tests):
- ✅ `filters_default_is_empty` - Verifies empty default
- ✅ `filters_with_exclude_ids_filters_beads` - Verifies field can be set
- ✅ `br_cli_bead_store_ready_filters_by_exclude_ids` - Integration test for BrCliBeadStore
- ✅ `bf_cli_bead_store_ready_filters_by_exclude_ids` - Integration test for BfCliBeadStore

## Acceptance Criteria Status
- [x] Filters has exclude_ids: HashSet<BeadId>, defaults to empty
- [x] Both BeadStore::ready() implementations filter it out
- [x] Unit test: ready() with exclude_ids containing an otherwise-ready bead's id returns it excluded
- [x] No existing test regresses (all 41 tests pass)
- [x] Did not touch the Strand trait or explore.rs

## Conclusion
No code changes were required. The feature was already implemented and tested correctly.
