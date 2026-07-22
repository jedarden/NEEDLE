# P7.1a Cleanup Liveness Bug: pane_pid vs Child PID Indirection

## Executive Summary

The P7.1a bug was a critical liveness detection failure in `needle cleanup` that killed live worker sessions on 2026-07-19 (armor-p6a, needle-supervisor). The bug shipped undetected because existing unit tests constructed `TmuxSession` structs directly, bypassing the exact PID indirection layer where the bug lived.

## The Bug: pane_pid vs Actual Child Process PID

### Real tmux Session Launch Structure

When NEEDLE launches a worker via `launch_in_tmux()`, it creates a tmux session with:

```bash
NEEDLE_INNER=1 /path/to/needle run --workspace ... --count 1 ... 2>> ~/.needle/logs/session.stderr.log
```

**Critical detail**: The output redirection (`2>>`) defeats bash's last-command exec optimization. This means:

1. **pane_pid** (from tmux's `#{pane_pid}`) = shell wrapper PID (e.g., bash PID 1234)
2. **Actual needle process PID** = child of shell wrapper (e.g., needle PID 5678)

### Process Tree Structure

```
tmux server (PID 1)
  └─ bash -c "NEEDLE_INNER=1 needle run ... 2>> log" (PID 1234) ← pane_pid points here
      └─ needle run --workspace ... --count 1 (PID 5678) ← actual worker process
```

### The Original Bug (Pre-P7.1a Fix)

The original `filter_sessions_for_cleanup()` logic was:

```rust
// BUGGY VERSION (pre-P7.1a)
sessions
    .iter()
    .filter(|s| {
        s.pid.map_or(true, |pane_pid| !live_pids.contains(&pane_pid))
    })
```

**Problem**: This directly checked if `pane_pid` (the shell wrapper) was in `live_pids`. But:

- `pane_pid` = shell wrapper PID (1234)  
- `live_pids` contains actual needle process PIDs (5678)
- Check: `live_pids.contains(1234)` → **false** (even though needle is running!)
- Result: Session incorrectly marked as orphaned and killed

### The Fix

The fixed version walks the process tree:

```rust
// FIXED VERSION (post-P7.1a)
s.pid.map_or(true, |pane_pid| {
    match find_needle_process_in_tree(pane_pid) {
        Some(needle_pid) => !live_pids.contains(&needle_pid),
        None => true, // No needle run found in tree → orphaned
    }
})
```

Now:
1. Start with `pane_pid` (1234 - shell wrapper)
2. Walk process tree via `find_all_descendants(1234)`
3. Find needle process at PID 5678
4. Check `live_pids.contains(5678)` → **true**
5. Result: Session correctly preserved as live

## Why Tests Didn't Catch It

### Unit Tests (src/cli/mod.rs)

The authoritative cleanup tests construct `TmuxSession` structs directly:

```rust
let sessions = vec![
    TmuxSession {
        name: "needle-claude-alpha".to_string(),
        created: "20240101T120000".to_string(),
        status: "detached".to_string(),
        pid: Some(1001), // Direct PID assignment
    },
    // ...
];
```

**Key bypass**: These tests set `pid: Some(1001)` where 1001 is the actual needle process PID, NOT a shell wrapper PID. This completely bypasses the pane_pid→child indirection.

When the test code runs:
```rust
let targets = filter_sessions_for_cleanup(&sessions, &live_pids, false, &None);
```

With `live_pids = {1001}`:
- Original buggy code: `live_pids.contains(1001)` → **true** ✅ (test passes by accident)
- Fixed code: Same result ✅

**The test passes for the wrong reason** - it never tests the shell-wrapper→child lookup because the PID field is already the child PID.

### Integration Tests (tests/cleanup_liveness_regression.rs)

The integration tests attempt to use real tmux sessions but are disabled (`#[ignore]`):

```rust
#[test]
#[cfg(unix)]
#[ignore = "Requires real needle run processes - covered by unit tests"]
fn regression_cleanup_no_flags_removes_only_dead_sessions() {
    // Test implementation...
}
```

**Problem**: These tests create sessions like:
```rust
Command::new("tmux")
    .args(["new-session", "-d", "-s", session_name, "sleep", "3600"])
    .spawn()?;
```

This creates `sleep 3600` directly (no shell wrapper), so pane_pid IS the child process PID - again bypassing the indirection.

### The P7.1a Regression Test

The P7.1a test (lines 540-692 in cleanup_liveness_regression.rs) is the **only** test that correctly reproduces the bug:

```rust
Command::new("tmux")
    .args([
        "new-session",
        "-d",
        "-s",
        session_name,
        &format!("NEEDLE_INNER=1 sleep 30 2>> {}", test_log),
    ])
    .status()?;
```

**Critical**: This uses the exact command shape as `launch_in_tmux()`:
- `NEEDLE_INNER=1` prefix
- `2>> log` output redirection
- Produces shell-wrapper→child split

The test then:
1. Gets `pane_pid` from tmux (shell wrapper PID)
2. Verifies `pane_pid` is a shell process (cmdline contains NEEDLE_INNER)
3. Runs `needle cleanup` (no flags)
4. Asserts session is **NOT** removed (because it's live)

## What a Real 'tmux new-session' Invocation Looks Like

### Via launch_in_tmux() (Production)

```bash
tmux new-session -d -s needle-claude-alpha \
    "NEEDLE_INNER=1 /home/coding/.local/bin/needle run --workspace /home/coding/NEEDLE --count 1 --identifier alpha 2>> /home/coding/.needle/logs/needle-claude-alpha.stderr.log"
```

Process tree:
```
tmux: server (PID 1)
  └─ bash -c "NEEDLE_INNER=1 needle run ... 2>> log" (PID 1234) ← pane_pid
      └─ needle run --workspace ... (PID 5678) ← actual worker
```

### Via Old Integration Test Pattern (Bypassed Bug)

```bash
tmux new-session -d -s needle-test-cleanup-live sleep 3600
```

Process tree:
```
tmux: server (PID 1)
  └─ sleep 3600 (PID 9999) ← pane_pid = child (no wrapper!)
```

**No indirection** - pane_pid IS the actual process, so the bug never manifests.

## Key Functions

### find_needle_process_in_tree() (lines 1290-1301)

Walks process tree from `pane_pid` to find actual needle process:

```rust
fn find_needle_process_in_tree(root_pid: u32) -> Option<u32> {
    if is_needle_run_process(root_pid) {
        return Some(root_pid);
    }
    
    let descendants = find_all_descendants(root_pid);
    descendants
        .into_iter()
        .find(|&pid| is_needle_run_process(pid))
}
```

### scan_needle_processes() (lines 4264-4349)

Scans /proc for actual needle run processes, excluding shell wrappers:

```rust
// Filter out shell wrapper processes
if cmdline.starts_with("bash -c")
    || cmdline.starts_with("sh -c")
    || cmdline.starts_with("/bin/bash -c")
    || cmdline.starts_with("/bin/sh -c")
{
    continue; // Skip shell wrappers - only want actual needle processes
}
```

## Timeline

### 2026-07-19: Incident
- bare `needle cleanup` killed live sessions armor-p6a and needle-supervisor
- Root cause: pane_pid (shell) not in live_pids (actual needle PIDs)
- Result: 2 workers killed, production disruption

### Post-Incident: P7.1a Fix
- Added `find_needle_process_in_tree()` to walk process tree
- Updated `filter_sessions_for_cleanup()` to use tree-walking
- Added P7.1a regression test with real tmux session using correct command shape

### Current State (2026-07-22)
- Unit tests: Still use constructed structs (pass for wrong reason)
- Integration tests: Still disabled (`#[ignore]`)
- P7.1a test: Only test that actually reproduces the bug scenario
- ADR-003: Documents the incident and fix

## Lessons Learned

### Testing Gaps

1. **Struct construction bypasses real indirection**: Direct `TmuxSession { pid: Some(x) }` construction skips the pane_pid→child lookup layer
2. **Integration tests disabled**: Real tmux tests marked `#[ignore]` due to complexity
3. **Only P7.1a test reproduces bug**: Uses exact production command shape with NEEDLE_INNER + output redirection

### Fix Verification

For future PID-indirection bugs:
1. **Test with real sessions**, not constructed structs
2. **Match production command shape exactly**, including shell wrappers
3. **Verify pane_pid != child PID** in test assertions
4. **Walk process trees** rather than assuming direct PID correspondence

## References

- ADR-003: Full incident postmortem for armor-p6a/needle-supervisor kill
- tests/cleanup_liveness_regression.rs: Integration tests (lines 540-692 for P7.1a test)
- src/cli/mod.rs: 
  - Lines 1290-1301: `find_needle_process_in_tree()`
  - Lines 1537-1580: `filter_sessions_for_cleanup()` (with fix)
  - Lines 4264-4349: `scan_needle_processes()` (excludes shell wrappers)
  - Lines 1010-1076: `launch_in_tmux()` (creates shell wrapper via output redirection)
  - Lines 6033-6265: Unit tests (constructed structs - bypass indirection)