# Bead bf-12bim Summary: Zombie Reaping Fix (GH #12)

## Status: Already Completed

The zombie reaping fix described in this bead was **already implemented** under bead `bf-z3yp0` (GitHub issue jedarden/NEEDLE#12). This bead (`bf-12bim`) is a tracking bead created to document and verify the completed work.

## Implementation Details

### Fix Location
- **File**: `src/supervisor/mod.rs`
- **Function**: `reap_zombie_children()` (lines 534-547)
- **Called from**: `Supervisor::tick()` at line 309

### Implementation

The fix adds a zombie child reaping sweep at the top of every supervisor tick:

```rust
async fn tick(&mut self) -> Result<bool> {
    reap_zombie_children();  // Line 309
    // ... rest of tick logic
}
```

The `reap_zombie_children()` function uses `libc::waitpid(-1, &mut status, libc::WNOHANG)` to reap any exited worker children without blocking:

```rust
#[cfg(unix)]
fn reap_zombie_children() {
    reap_children_matching(-1);
}
```

The shared `reap_children_matching()` function (lines 560-575) implements the non-blocking reap loop:
- Loops until `waitpid` returns 0 (no more children to reap) or -1 (ECHILD/no children)
- Logs each reaped child PID for debugging
- Platform-safe: no-op on non-Unix platforms

### Acceptance Criteria Met

✅ **Supervisor::tick() reaps exited workers within one tick**
   - `reap_zombie_children()` called at top of every `tick()` iteration
   - Uses `WNOHANG` to reap without blocking
   - Prevents zombie accumulation for lifetime of supervisor daemon

✅ **Regression test exists**
   - Test: `reap_zombie_children_reaps_an_exited_child()` (lines 667-721)
   - Spawns `true` command, waits for zombie state, verifies reap
   - Scoped to specific PID to avoid cross-test collision in `cargo test --lib`

✅ **No change to spawn_worker detach model**
   - `spawn_worker()` still uses `setsid` + `process_group(0)` for daemonization
   - This is a missing-reap fix, not a re-architecture

✅ **Tests pass**
   - `cargo test --lib` passes (regression test confirmed passing)
   - No new clippy warnings introduced

## Git History

- `81b0995` - "fix(bf-z3yp0): reap zombie supervisor children + zombie-aware is_pid_alive"
- `f81e6bc` - "fix(bf-z3yp0): scope the reap-sweep test to avoid cross-test collision"
- `ffa3ecf` (HEAD) - "docs(bf-12bim): document that zombie reaping fix was already completed"

## Verification

The fix was verified against the production report (jarvis-laboratories, ~900-bead monorepo cutover):
- **Before fix** (v0.2.12/fad0b50): 22 zombie processes after ~15 minutes
- **After fix**: Zombies reaped within one poll interval (default 10 seconds)

## Design Rationale

Full rationale documented in ADR-010 (`docs/adr/010-supervisor-zombie-reaping.md`):
- Alternatives considered: double-fork+reparent to init, retain Child handles
- Chosen approach: waitpid sweep with WNOHANG (minimal change, no new state)
- Safety: Cannot race with other `.wait()` calls (dispatch/telemetry/canary run in separate PID tree under worker process)

## Related Work

- GitHub issue: https://github.com/jedarden/NEEDLE/issues/12
- ADR-010: `docs/adr/010-supervisor-zombie-reaping.md`
- plan.md Phase 14.1 tracking
