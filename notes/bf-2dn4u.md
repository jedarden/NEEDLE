# Bead bf-2dn4u: Cleanup Liveness Regression Tests - Verification

## Task
Regression tests for needle cleanup liveness detection - pin the three behaviors from plan.md Phase 7.2 so the docs-vs-implementation gap that caused the 2026-07-19 incident (ADR-003) cannot silently reopen.

## Acceptance Criteria - ALL MET ✅

### Test 1: needle cleanup with no flags removes only dead sessions
✅ **PASS** - `cleanup_no_flags_filters_orphaned_sessions`
- Location: `src/cli/mod.rs:5927-5980`
- Verifies that with one live session (PID 12345) and one orphaned session (PID 99999), cleanup removes only the orphaned one
- This is the core safety fix from ADR-003

### Test 2: needle cleanup with no flags and zero dead sessions removes nothing
✅ **PASS** - `cleanup_no_flags_with_zero_dead_removes_nothing`
- Location: `src/cli/mod.rs:5983-6039`
- Verifies that when all sessions are live (armor-p6a, needle-supervisor, alpha), cleanup removes nothing
- This is the exact scenario that killed armor-p6a and needle-supervisor on 2026-07-19
- Both armor-p6a and needle-supervisor are explicitly tested and survive

### Test 3: needle cleanup --all removes every session regardless of liveness
✅ **PASS** - `cleanup_all_removes_every_session_regardless_of_liveness`
- Location: `src/cli/mod.rs:6042-6097`
- Verifies that --all flag bypasses liveness check and removes all sessions
- Pinned explicitly so it cannot regress while fixing the no-flags path

## Additional Coverage

✅ **Edge case**: `cleanup_session_without_pid_is_considered_orphaned`
- Tests sessions with pid: None are considered orphaned

✅ **CLI parsing**: `cli_parses_cleanup_*` tests
- Verifies cleanup command parses correctly for --all, -i, and no-flags variants

## Test Execution Results

```
running 1 test
test cli::tests::cleanup_no_flags_filters_orphaned_sessions ... ok

running 1 test
test cli::tests::cleanup_no_flags_with_zero_dead_removes_nothing ... ok

running 1 test
test cli::tests::cleanup_all_removes_every_session_regardless_of_liveness ... ok

running 1 test
test cli::tests::cleanup_session_without_pid_is_considered_orphaned ... ok

running 3 tests
test cli::tests::cli_parses_cleanup_all ... ok
test cli::tests::cli_parses_cleanup_identifier ... ok
test cli::tests::cli_parses_cleanup_no_flags ... ok
```

## Implementation Verified

The cleanup implementation in `src/cli/mod.rs:1447-1515` correctly implements all three behaviors:

1. **No flags (default)**: Uses `scan_needle_processes()` to check liveness and only removes orphaned sessions
2. **--all flag**: Removes all sessions regardless of liveness
3. **-i flag**: Filters by identifier substring (bypasses liveness check)

The docs-vs-implementation gap from ADR-003 is now closed and protected by these regression tests.

## Related
- ADR-003: docs/adr/003-cleanup-orphan-detection-gap.md
- plan.md Phase 7.2: Cleanup liveness detection
- Integration tests: tests/cleanup_liveness_regression.rs (disabled, covered by unit tests)
