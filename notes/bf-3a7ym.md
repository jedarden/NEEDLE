# Bead bf-3a7ym: Identify Module for cleanup_heartbeat_file

## Task
Identify the appropriate module for `cleanup_heartbeat_file` function placement.

## Findings

The `cleanup_heartbeat_file` function **already exists** in the correct location.

### File Path
**`/home/coding/NEEDLE/src/health/mod.rs`**

### Implementation Details

The function is implemented in two forms within the health module:

1. **Standalone utility function** (lines 860-890):
   - Signature: `pub fn cleanup_heartbeat_file(path: &Path) -> Result<()>`
   - Purpose: Remove heartbeat file at a given path
   - Error handling: Best-effort (logs errors, returns Ok)
   - Used by: External callers who have a specific path to clean up

2. **Method on HealthMonitor** (lines 280-312):
   - Signature: `pub fn cleanup_heartbeat_file(&self) -> Result<()>`
   - Purpose: Remove this worker's own heartbeat file
   - Uses: `self.heartbeat_path()` to get the path
   - Called by: `stop()` method during graceful shutdown

### Module Structure

The `src/health/mod.rs` module is the **core heartbeat module** containing:

- **HeartbeatData**: JSON structure for on-disk heartbeat files
- **HealthMonitor**: Main health monitoring struct
- **SharedHeartbeatState**: Thread-safe state between worker and emitter
- **emitter_loop()**: Background thread that writes heartbeats periodically
- **Peer detection**: `detect_stale_peers()`, `check_pid_alive()`
- **Supervisor detection**: `detect_supervisor()`, `detect_supervisor_direct()`
- **Cleanup functions**: Both `cleanup_heartbeat_file` variants

### Acceptance Criteria Status

✅ **Identified the module containing heartbeat-related functions**
- Module: `src/health/mod.rs`
- All heartbeat functionality is centralized in this module

✅ **Confirmed the module structure and existing heartbeat code**
- The health module has comprehensive heartbeat functionality:
  - Heartbeat file creation and atomic writes
  - Periodic heartbeat emission (every 30s)
  - Stale peer detection
  - Heartbeat cleanup on shutdown
  - Supervisor presence detection
  - Cross-workspace support

✅ **Documented the file path where the function should be added**
- Path: `/home/coding/NEEDLE/src/health/mod.rs`
- Function already present (both forms)
- No action needed - function is in the correct location

### Integration Points

The `cleanup_heartbeat_file` function is integrated with:

1. **Shutdown signal handler** (in `src/worker/mod.rs`):
   - Called during graceful shutdown (SIGTERM)
   - Ensures heartbeat file is removed when worker exits

2. **HealthMonitor::stop()** method:
   - Automatically calls cleanup during normal shutdown
   - Part of the Drop trait implementation

3. **Peer monitoring** (in `src/peer/mod.rs`):
   - Uses similar cleanup logic (`remove_heartbeat_file`)
   - Removes stale heartbeat files from crashed workers

### Testing

Comprehensive test coverage exists in `src/health/mod.rs`:
- `cleanup_heartbeat_file_removes_existing_file` (line 2079)
- `cleanup_heartbeat_file_ok_when_file_missing` (line 2094)
- `cleanup_heartbeat_file_logs_errors_on_failure` (line 2115)
- `cleanup_heartbeat_file_with_heartbeat_path` (line 2138)
- Plus tests for the HealthMonitor method variant (lines 2508-2632)

## Conclusion

The `cleanup_heartbeat_file` function is properly co-located with all other heartbeat-related code in the `health` module. This module is the single source of truth for heartbeat functionality in NEEDLE, and the function placement is correct.

No code changes are needed. The function is in the appropriate module and properly integrated into the shutdown signal handler.
