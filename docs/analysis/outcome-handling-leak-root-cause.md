# Root Cause Analysis: needle-6d76f548 Outcome Handling Leak

## Summary

Live evidence from 2026-08-23 shows 5 beads with `exit_code=0`, `outcome=success` that remained `in_progress` for 25+ hours, despite the postcondition-enforcement fix in commit 7a63762. The root cause is NOT a bug in the outcome handling code itself, but rather a **missing postcondition when the worker process is killed** between dispatch completion and outcome handling.

## Timeline Analysis

### The Evidence
- 5 beads stuck in `in_progress` for 25+ hours
- All 5 have `exit_code=0`, `outcome=success` in trace metadata
- 3 of 5 have bash errors: `shell-init: error retrieving current directory: getcwd: cannot access parent directories: No such file or directory`
- 3 of 5 share **identical `captured_at` timestamps** (`2026-08-23T13:26:11`) despite wildly different durations:
  - irm-4687f145: 469530 ms (7.8 minutes)
  - irm-aff8570f: 853728 ms (14.2 minutes)  
  - irm-ed9ec7e8: 1860063 ms (31 minutes)

### The Root Cause

The identical `captured_at` timestamps are the smoking gun. Looking at `src/dispatch/mod.rs:2159`:

```rust
// In dispatch ExecutionResult::new()
captured_at: chrono::Utc::now(),  // Set when trace is written
```

This timestamp is written:
1. **AFTER** the agent exits (in `do_execute()`)
2. **BEFORE** the worker enters HANDLING state
3. **BEFORE** `do_handle()` calls the outcome handler

The fact that 3 different workers with wildly different durations have the **exact same second** in `captured_at` proves that trace metadata was NOT written immediately after agent exit. Instead, it was written by a **batch operation** (likely a supervisor sweep or post-mortem handler) at `2026-08-23T13:26:11`.

### The Failure Mode

The worker lifecycle is:
1. `do_execute()` → agent exits → trace written → **`exec_output = Some((output, was_interrupted))`**
2. `set_state(WorkerState::Handling)` ← **CRITICAL CHECKPOINT**
3. Main loop calls `do_handle()` → outcome handler → `apply_bead_action()` → **bead released**

If the worker is killed (**SIGKILL, OOM, supervisor kill**) at step 2, **the bead remains `in_progress` forever** because:
- The trace file exists with `outcome=success` (misleading observers)
- The outcome handler never ran
- `apply_bead_action()` never released the bead
- No watchdog or cleanup mechanism exists

## Why the Fix in 7a63762 Didn't Help

Commit 7a63762 fixed the outcome handler to release beads when `show()` times out or errors (which covers the vanished-cwd case). However, **it only helps if the outcome handler actually runs**. If the worker is killed before `do_handle()`, the handler never executes at all.

## The Bash Error

The error `shell-init: error retrieving current directory: getcwd: cannot access parent directories` occurs when:
- A bash shell has `cd`'d into a directory
- That directory is deleted (by another process)
- The shell tries to call `getcwd()` (e.g., in prompt rendering)

This suggests the workspace directory was deleted while workers were still running, possibly by:
- A supervisor cleanup operation
- A concurrent workspace rebuild
- Manual cleanup by an operator
- An OOM killer sweep that targeted the workspace path

## The Fix

### Option 1: Supervisor/Watchdog (Recommended)

Add a watchdog in the supervisor/dispatch layer that:
1. Detects workers with `in_progress` beads that haven't updated heartbeat within timeout
2. Checks if the worker process is still alive
3. If dead, releases the bead with a `worker_died` reason
4. Emits `BeadReleased` telemetry for observability

### Option 2: Worker Self-Recovery

Add a startup check in the worker that:
1. On boot, queries for beads assigned to this worker that are `in_progress`
2. For each such bead, checks if trace file exists with `outcome=success`
3. If trace says success but bead is still `in_progress`, releases it
4. Emits `BeadReleased` telemetry explaining the recovery

### Option 3: Kernel Crash Safety (Not Feasible)

Use `prctl(PR_SET_PDEATHSIG)` on Linux to have the kernel deliver SIGTERM instead of SIGKILL to children when the parent dies. This won't work because:
- The supervisor needs SIGKILL for reliable cleanup
- OOM killer is unavoidable
- This doesn't help with manual workspace deletion

## Regression Test

The existing regression test `handle_success_releases_bead_when_workspace_vanishes` tests the vanished-cwd case correctly. However, it **only tests the outcome handler**, not the complete worker lifecycle. We need an integration test that simulates:
1. Worker completes `do_execute()` 
2. Worker is killed before `do_handle()`
3. Verify bead is still stuck (proves the bug)
4. Restart worker
5. Verify recovery mechanism releases the bead

## Mitigation

Until a proper fix is implemented, operators should:
1. Monitor for beads `in_progress` for >2 hours without heartbeat updates
2. Manually release stuck beads with: `bead release <bead-id> --if-revision`
3. Check if shipped work exists before releasing: `git log --oneline --all | grep <bead-id>`

## Related Beads

- needle-3386daef: Original postcondition enforcement fix
- needle-6d76f548: This investigation and fix
