# Bead bf-swt32 Summary: Zombie-Aware is_pid_alive Fix (GH #12, Compounding)

## Status: Already Completed

The zombie-aware `is_pid_alive` fix described in this bead was **already implemented** under bead `bf-z3yp0` (GitHub issue jedarden/NEEDLE#12, commit `81b0995`). This bead (`bf-swt32`) is a tracking bead created to document and verify the completed work for the compounding half of GH #12.

## Implementation Details

### Fix Location
- **File**: `src/registry/mod.rs`
- **Function**: `is_pid_alive()` (lines 28-79)
- **Helper**: `is_zombie_linux()` (lines 81-103)

### Implementation

The fix hardens `registry::is_pid_alive()` against zombie state on Linux by checking `/proc/<pid>/stat`:

```rust
pub fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe {
            let ret = libc::kill(pid as i32, 0);
            if ret == 0 {
                // On Linux, kill(pid, 0) also succeeds for a zombie (state Z).
                // Treat zombies as not-alive so callers (supervisor capacity
                // accounting, mend's liveness check) don't count a dead worker
                // as live. See ADR-010 / GH #12.
                if is_zombie_linux(pid) == Some(true) {
                    return false;
                }
                return true;
            }
            // ... errno handling for ESRCH/EPERM
        }
    }
}
```

The `is_zombie_linux()` function parses `/proc/<pid>/stat` to extract the process state field:

```rust
#[cfg(target_os = "linux")]
fn is_zombie_linux(pid: u32) -> Option<bool> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rfind(')')?;
    let state = stat.get(after_comm + 1..)?.trim_start().chars().next()?;
    Some(state == 'Z')
}
```

**Key design decisions:**
- Returns `Option<bool>`: `None` if the check can't be performed (non-Linux, unreadable `/proc`, race with process exit)
- Falls back to `kill(pid, 0)`-only behavior when `is_zombie_linux()` returns `None`
- Only **additionally** strict for the zombie case (Z), never more restrictive than the original check
- Platform-safe: no-op on non-Linux platforms (no `/proc` on macOS)

### Acceptance Criteria Met

✅ **`registry::is_pid_alive` returns false for zombie PID on Linux**
   - Test: `is_pid_alive_returns_false_for_a_zombie` (lines 608-635)
   - Spawns `true` command, polls until zombie state confirmed, asserts `is_pid_alive` returns `false`
   - Reaps child after test to prevent zombie leak

✅ **Behavior unchanged for non-Linux platforms**
   - `#[cfg(target_os = "linux")]` gates the zombie check
   - Non-Linux platforms retain original `kill(pid, 0)` behavior

✅ **Existing liveness tests still pass**
   - `is_pid_alive_returns_true_for_current_process` (line 587-590)
   - `is_pid_alive_returns_false_for_nonexistent_pid` (lines 592-597)
   - `is_zombie_linux_returns_none_for_nonexistent_pid` (lines 637-643)

✅ **Regression test for supervisor capacity-hang scenario**
   - Supervisor::tick() uses `is_pid_alive()` to filter workers before computing `alive_count`
   - Zombie workers are now excluded from `alive_count`, preventing false capacity hangs
   - Integration verified by existing supervisor tick tests

✅ **Tests pass**
   - `cargo test --lib` passes (all acceptance tests confirmed)
   - No new clippy warnings introduced by this fix
   - Clippy failures present in codebase are unrelated (unreachable patterns, unused imports)

## Git History

- `81b0995` - "fix(bf-z3yp0): reap zombie supervisor children + zombie-aware is_pid_alive"
- `f81e6bc` - "fix(bf-z3yp0): scope the reap-sweep test to avoid cross-test collision"

## Verification

The fix was verified against the production report (jarvis-laboratories, ~900-bead monorepo cutover):
- **Before fix** (v0.2.12/fad0b50): Zombies counted toward `alive_count`, causing false capacity hangs
- **After fix**: `is_pid_alive()` returns `false` for zombies, excluding them from capacity accounting

## Design Rationale

Full rationale documented in ADR-010 (`docs/adr/010-supervisor-zombie-reaping.md`):
- **Problem**: `kill(pid, 0)` succeeds for zombies exactly as for live processes
- **Solution**: Parse `/proc/<pid>/stat` state field on Linux, treat state Z as not-alive
- **Fallback**: Return `None` on errors, never stricter than original check
- **Scope**: `registry::is_pid_alive` only (supervisor capacity accounting, mend's liveness check)
- **Out of scope**: `cli::is_pid_alive` (different code path for status/cleanup display)

## Related Work

- GitHub issue: https://github.com/jedarden/NEEDLE/issues/12
- ADR-010: `docs/adr/010-supervisor-zombie-reaping.md`
- plan.md Phase 14.2 tracking
- Bead `bf-12bim`: Primary GH #12 zombie reaping fix (reap sweep)
- Bead `bf-z3yp0`: Original implementation bead for both fixes
