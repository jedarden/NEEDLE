# Bead bf-1ynu9: Supervisor spawns workers via current_exe() instead of PATH lookup

## Task Description
Fix GitHub issue #11 where `Supervisor::spawn_worker` builds its child via `Command::new("needle")`, a bare $PATH lookup. The issue reported that in the reporter's migration, a different legacy tool occupied the name 'needle' on PATH, so supervise silently spawned the wrong binary.

## Resolution Status
**ALREADY COMPLETE** - Implemented in commit e97e88a on 2026-07-28

## Implementation Details

The fix was implemented as part of ADR-009 (External-Adopter Hardening) along with fixes for GH #7-#10:

### 1. New Config Field
`WorkerConfig::worker_binary_path: Option<PathBuf>` - allows explicit override of the worker binary path

### 2. Binary Resolution Function
`resolve_worker_binary()` in `src/supervisor/mod.rs` (lines 88-106):
- Prefers explicit `worker_binary_path` override if set
- Falls back to `std::env::current_exe()` (default behavior)
- Final fallback to bare `"needle"` PATH lookup only if `current_exe()` fails
- Logs a warning when falling back to PATH lookup

### 3. Supervisor Integration
- `Supervisor` struct stores resolved binary in `worker_binary: PathBuf` field (line 84)
- Resolved once at startup in `Supervisor::new()` (line 137)
- Logs resolved path at startup for operator visibility (lines 138-141)
- Used in `spawn_worker()` via `Command::new(&self.worker_binary)` (line 455)

### 4. Config Wiring
- `run_supervisor()` passes `config.worker.worker_binary_path.clone()` to supervisor config (line 603)
- Field properly integrated with config system and tilde expansion

### 5. Test Coverage
Three comprehensive tests verify the behavior:
- `resolve_worker_binary_uses_explicit_override_when_set` - confirms override takes priority
- `resolve_worker_binary_defaults_to_current_exe` - confirms current_exe() is default (not PATH lookup)
- `supervisor_config_default_has_no_worker_binary_override` - confirms default behavior

All tests pass ✅

## Documentation
- ADR-009 documents the rationale and implementation
- `docs/adr/009-external-adopter-hardening.md` has full details
- References GitHub issue jedarden/NEEDLE#11

## Verification
```bash
cargo test --lib supervisor::tests::resolve_worker_binary
# All tests pass
```
