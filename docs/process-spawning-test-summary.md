# Process-Spawning Test Summary

## Quick Reference

- **Total `Command::new` sites**: 49
- **In test code**: 35
- **In production/helpers**: 14

## Test Breakdown

### By Category

| Category | Count | Percentage |
|----------|-------|------------|
| Process-spawning (generic) | 20 | 57% |
| Worker-lifecycle (NEEDLE) | 12 | 34% |
| Other (helpers) | 3 | 9% |
| **Total** | **35** | **100%** |

### By Process Type

| Process | Count | Tests |
|---------|-------|-------|
| git | 25 | scratch_sweep (6), commit_hook (2), ci (2), mitosis (8), shipped_work (4), timeout_context helpers (3) |
| sh/bash | 8 | telemetry (5), validation/mod (5), pulse (3), dispatch (2), strand agents (6) |
| needle | 5 | canary (3), supervisor (4), upgrade (3), hoop_hooks (6), cli (1) |
| bead | 6 | workspace_equality (6), bead_store helpers |
| claude | 4 | strand/resolve (2), resolve/mod (2) |
| agent (custom) | 4 | predispatch (4), strand agents (4) |
| cargo | 8 | test_output (8), cargo_test helpers |
| Other | 5 | sqlite3 (1), true (2), which/command (2) |

## Migration Impact

### Move to Integration Target (--test): 48 tests
- **High priority**: 24 tests (worker lifecycle, agent spawning)
- **Medium priority**: 24 tests (external CLI dependencies)

### Keep in --lib: 29 tests
- Git-only operations: 22 tests
- Standard shell utilities: 5 tests
- Unix-specific tests: 2 tests

## Files Requiring Migration

### Complete Migration Recommended
1. src/canary/mod.rs
2. src/supervisor/mod.rs
3. src/upgrade/mod.rs
4. src/dispatch/mod.rs (agent spawn tests)
5. src/strand/resolve.rs
6. src/strand/reflect.rs
7. src/strand/weave.rs
8. src/strand/unravel.rs
9. src/resolve/mod.rs
10. src/workspace_equality.rs
11. src/telemetry/mod.rs (hook tests)
12. src/hoop_hooks.rs
13. src/validation/predispatch.rs
14. src/validation/mod.rs
15. src/strand/pulse.rs
16. src/test_output.rs

### Partial Migration Recommended
1. src/cli/mod.rs (1 test to move, 1 helper is production code)
2. src/registry/mod.rs (Unix-specific test, could stay with cfg attribute)

### No Migration Needed
1. src/scratch_sweep.rs (git-only)
2. src/commit_hook.rs (git-only)
3. src/ci.rs (git-only)
4. src/mitosis/timeout_context.rs (git-only)
5. src/validation/shipped_work.rs (git-only)

---

*Generated: 2026-08-28*
