# Bead bf-j1b8l: Bead-Id Trailer Injection Race Fix

## Implementation Status: COMPLETE

The fix for the concurrent Bead-Id trailer injection race condition was implemented in commit `986ae98` ("feat(needle-bf-4390q): verify test output capture implementation").

## Acceptance Criteria Verified

### 1. Advisory Lock (flock)
✅ **DONE** - Implemented in `src/commit_hook.rs`:
- `acquire_flock()` function (lines 194-225)
- Lock file path: `<workspace>/.git/needle-trailer.lock` (line 187)
- Lock scoped only to the verify → amend sequence (released when `_lock` drops out of scope)
- 10-second timeout with 50ms polling interval

### 2. Identity Check
✅ **DONE** - Implemented in `src/commit_hook.rs`:
- `git_head_subject()` helper function (lines 166-180)
- Verifies commit subject contains bead ID before amending (lines 88-101)
- If HEAD doesn't match this bead, skips injection to avoid mislabeling
- Logs a warning when skipping for investigation

### 3. Regression Test  
✅ **DONE** - Implemented in `src/commit_hook.rs`:
- `concurrent_inject_never_cross_tags` test (lines 339-454)
- Simulates the exact race condition described in the bead
- Verifies workers never cross-tag each other's commits
- Additional helper tests: `identity_check_skips_mismatched_commit`, `identity_check_allows_matched_commit`

## Test Results
All 5 tests in `commit_hook::tests` pass:
```
test commit_hook::tests::already_has_trailer_logic ... ok
test commit_hook::tests::empty_head_means_no_op ... ok
test commit_hook::tests::identity_check_allows_matched_commit ... ok
test commit_hook::tests::identity_check_skips_mismatched_commit ... ok
test commit_hook::tests::concurrent_inject_never_cross_tags ... ok
```

## Race Condition Prevented

**Before fix:**
- Worker A commits (HEAD=A1)
- Worker B commits (HEAD=B1 on top of A1)  
- Worker A reads HEAD=B1, verifies B1 != pre_dispatch_head ✓
- Worker A amends B1 with Bead-Id:A → **MISLABELING**

**After fix:**
- Worker A commits (HEAD=A1)
- Worker B commits (HEAD=B1 on top of A1)
- Worker A acquires flock, reads HEAD=B1, checks subject contains "bf-A"
- Subject contains "bf-B", not "bf-A" → **Worker A skips injection**
- Worker B acquires flock, reads HEAD=B1, checks subject contains "bf-B"
- Subject contains "bf-B" → Worker B amends with Bead-Id:B ✓

## Implementation Details

- **Lock mechanism**: Uses `fs2::FileExt` for cross-process advisory locking
- **Identity verification**: Leverages NEEDLE commit convention (`fix(needle-XYZ): ...`)
- **Error handling**: Non-fatal; logs warnings and returns Ok(()) on lock acquisition failure
- **Idempotency**: Checks for existing trailer before injection
- **Timeouts**: 10s for git operations, 30s for commit amend, 10s for flock acquisition

## Notes

- Fix is backward compatible with existing beads
- No changes required to HOOP's bead_commit_index
- Worker integration already calls `inject_bead_id_trailer` with proper timeout (30s)
- Follow-up about git notes was noted as "not required for this bead"

## Verification Date
2026-07-15
