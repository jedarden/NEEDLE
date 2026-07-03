# Bead bf-43ff: Add basic supervisor config struct

## Task Status: ✅ ALREADY COMPLETE

The supervisor config struct was already implemented in commit `277f1c6 feat(needle-bf-hkhz): implement supervisor config structure`.

## Acceptance Criteria Verification

All acceptance criteria are met:

1. ✅ Config struct exists in `src/config/mod.rs` (lines 1326-1378)
2. ✅ Has `heartbeat_path` field as `Option<PathBuf>`
3. ✅ Has `socket_path` field as `Option<PathBuf>`
4. ✅ Struct derives `Debug` and `Clone` (also `Serialize, Deserialize`)
5. ✅ Full rustdoc comments present

## Implementation Details

The `SupervisorConfig` struct in `src/config/mod.rs` includes:

- `heartbeat_path: Option<PathBuf>` - Path to supervisor's heartbeat file for liveness detection
- `socket_path: Option<PathBuf>` - Optional Unix domain socket path for IPC
- Default implementation with both fields defaulting to `None`
- Helper method `resolved_heartbeat_path()` for default path resolution
- Integrated into main `Config` struct (line 1710)
- Environment variable overrides supported via `NEEDLE_SUPERVISOR__HEARTBEAT_PATH` and `NEEDLE_SUPERVISOR__SOCKET_PATH`

## Documentation

The struct is well-documented with rustdoc comments explaining:
- Purpose: supervisor detection for graceful shutdown and resource cleanup
- Heartbeat file usage and default location
- Socket communication for IPC
- Example paths and configuration

No additional work was required for this bead.
