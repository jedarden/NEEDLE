# wait() Calls Audit - dispatch/mod.rs

**Date:** 2026-08-28  
**Purpose:** Establish baseline before ProcessGuard migration  
**Bead:** needle-3991bc15

## Summary

Found **4** direct `.wait()` calls in `src/dispatch/mod.rs` (note: task description mentioned 5, but only 4 exist in current codebase).

All calls are already using `ProcessGuard`, which provides safe process management.

---

## Detailed Inventory

### 1. Line 1581 - Main agent process wait (no deadline)

**Context:** `run_process()` method  
**Location:** Inside the "no deadlines" branch (when `!has_any_deadline`)

```rust
let status = ProcessGuard::new(child)
    .wait()
    .await
    .context("failed to wait for agent process")?;
kill_guard.disarm();
(status.code().unwrap_or(-1), None)
```

**Usage Pattern:** 
- Direct wait on newly created `ProcessGuard` wrapping the agent child process
- Used when no timeout enforcement is configured
- Followed by `kill_guard.disarm()` to prevent group kill on drop

**Error Handling:**
- Uses `.context()` for anyhow error chain
- Returns exit code or -1 if unavailable

**Notes:**
- This is the primary "happy path" wait for agent processes without timeouts
- Already properly wrapped with ProcessGuard

---

### 2. Line 1657 - Idle timeout reap

**Context:** `run_process()` method  
**Location:** Inside select! macro, idle timeout branch

```rust
// Idle timeout: kill the process group.
if pid > 0 {
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}
let _ = guard.get_mut().and_then(|c| c.start_kill().ok());
let _ = guard.wait().await;
kill_guard.disarm();
```

**Usage Pattern:**
- Called after `killpg()` to signal entire process group
- Followed by `start_kill()` on the direct child
- Result discarded with `let _`
- Followed by `kill_guard.disarm()`

**Error Handling:**
- Ignores errors (`let _`) - reap is best-effort after kill

**Notes:**
- Ensures zombie process is reaped after SIGKILL
- Part of idle timeout enforcement (no stdout/stderr activity)

---

### 3. Line 1690 - Hard deadline reap

**Context:** `run_process()` method  
**Location:** Inside select! macro, hard deadline branch

```rust
// Hard timeout: kill the process group.
if pid > 0 {
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}
let _ = guard.get_mut().and_then(|c| c.start_kill().ok());
let _ = guard.wait().await;
kill_guard.disarm();
```

**Usage Pattern:**
- Called after `killpg()` to signal entire process group
- Followed by `start_kill()` on the direct child
- Result discarded with `let _`
- Followed by `kill_guard.disarm()`

**Error Handling:**
- Ignores errors (`let _`) - reap is best-effort after kill

**Notes:**
- Ensures zombie process is reaped after SIGKILL
- Part of hard deadline enforcement (absolute wall-clock timeout)

---

### 4. Line 1782 - Transform process reap

**Context:** `run_process()` method  
**Location:** Transform cleanup logic (after timeout or error)

```rust
// Reap the killed transform before allowing its log
// writer to finish and before returning from dispatch.
let _ = t_guard.wait().await;
if feeder_drained {
    TransformOutcome::KilledAfterDrain
} else {
    TransformOutcome::KilledNoDrain
}
```

**Usage Pattern:**
- Waits on `t_guard` (ProcessGuard wrapping transform process)
- Called after `killpg()` on transform process group
- Result discarded with `let _`
- Used to determine outcome classification

**Error Handling:**
- Ignores errors (`let _`) - reap is best-effort after kill

**Notes:**
- Ensures transform zombie is reaped before returning from dispatch
- Part of output transform cleanup (e.g., needle-transform-claude)

---

## Key Observations

1. **All calls already use ProcessGuard** - No bare `child.wait()` calls found
2. **Three types of wait patterns:**
   - Primary wait (line 1581): normal completion, error handling matters
   - Reap after kill (lines 1657, 1690): best-effort cleanup, errors ignored
   - Transform reap (line 1782): best-effort cleanup, errors ignored
3. **Kill pattern consistent:**
   - `killpg()` on process group first
   - `start_kill()` on direct child
   - `wait()` to reap zombie
   - `disarm()` kill guard
4. **Line 1538 mentioned in task** - Not found in current code (may have been refactored or misnumbered)

---

## ProcessGuard Migration Readiness

**Current State:** Code is already using ProcessGuard correctly

**What may need migration:**
- Verify all wait() patterns match ProcessGuard API expectations
- Ensure proper error propagation paths
- Check if any wait() calls need special handling during migration

**Next Steps:**
- Review ProcessGuard implementation to verify API compatibility
- Identify any edge cases in current wait() usage
- Plan migration strategy if any changes are needed
