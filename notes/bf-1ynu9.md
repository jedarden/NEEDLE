# Bead bf-1ynu9: Fix GH #11 - Supervisor spawns workers via current_exe()

## Task Summary

Fix GitHub issue #11: Supervisor spawns workers via current_exe() instead of PATH lookup of 'needle'.

## Implementation Status

**ALREADY COMPLETED** in commit e97e88a ("fix: harden gates/spawn/bead-status per external adopter (GH #7-#11)")

The implementation includes:

1. **Config field added**: `WorkerConfig.worker_binary_path: Option<PathBuf>` in src/config/mod.rs
   - Defaults to `None` (uses `current_exe()`)
   - Allows explicit override when needed

2. **Resolution function**: `resolve_worker_binary()` in src/supervisor/mod.rs
   - Prefers explicit `worker_binary_path` override if set
   - Falls back to `std::env::current_exe()` by default
   - Final fallback to bare `"needle"` PATH lookup if `current_exe()` fails

3. **Supervisor integration**: 
   - `Supervisor` struct has `worker_binary: PathBuf` field (resolved once at startup)
   - `Supervisor::new()` resolves the binary and logs it at startup
   - `spawn_worker()` uses `&self.worker_binary` instead of hardcoded `"needle"`

4. **Tests added**:
   - `resolve_worker_binary_uses_explicit_override_when_set`
   - `resolve_worker_binary_defaults_to_current_exe`
   - `supervisor_config_default_has_no_worker_binary_override`
   - Config tests for `worker_binary_path` field

## Verification

All tests pass:
- `resolve_worker_binary` tests: 2 passed
- `worker_binary_path` config tests: 3 passed  
- Supervisor tests: 39 passed

## References

- GitHub issue #11: https://github.com/jedarden/NEEDLE/issues/11
- ADR-009: docs/adr/009-external-adopter-hardening.md
- Commit: e97e88a

## Conclusion

No code changes required - the fix was already implemented as part of the external adopter hardening work (ADR-009).
