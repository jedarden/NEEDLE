# Fix for bf-2unnq: cargo test --lib deadlock

## Problem
Four tests in `src/strand/explore.rs` were hanging indefinitely:
- `strand::explore::tests::deadlock_scenario_assigned_beads_allow_advancement`
- `strand::explore::tests::deadlock_scenario_excluded_and_assigned_beads_allow_advancement`
- `strand::explore::tests::deadlock_scenario_excluded_beads_allow_advancement`
- `strand::explore::tests::nonexistent_workspace_path_returns_no_work`

## Root Cause
These tests use `#[tokio::test]` which defaults to the `current-thread` runtime. The tests call `ExploreStrand::evaluate()`, which in turn calls `cleanup_orphaned_in_progress()` from the `mend` module. This function calls `HealthMonitor::check_pid_alive()` which performs a blocking system call (`libc::kill(pid, 0)`).

When running with the `current-thread` runtime, this blocking call can prevent the tokio executor from making progress, leading to a deadlock.

## Solution
Changed all 4 hanging tests from `#[tokio::test]` to `#[tokio::test(flavor = "multi_thread")]`. This uses the multi-threaded runtime, which can handle blocking system calls without deadlocking.

## Testing
All 4 tests now pass successfully:
```bash
cargo test --lib 'strand::explore::tests::deadlock_scenario' -- --nocapture
cargo test --lib 'strand::explore::tests::nonexistent_workspace_path_returns_no_work' -- --nocapture
```

Both commands complete in ~0.01s instead of hanging indefinitely.
