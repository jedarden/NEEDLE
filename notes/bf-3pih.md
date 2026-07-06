# Bead bf-3pih: Heartbeat File Cleanup Implementation

## Status: ✅ COMPLETE

This bead's acceptance criteria were already met by previous implementations in beads bf-34pp, bf-63o2, and bf-3oy8.

## Acceptance Criteria Verification

### ✅ 1. Create a cleanup function in HealthMonitor that removes the heartbeat file
**Location**: `src/health/mod.rs:281-308`

The `HealthMonitor::cleanup_heartbeat_file(&self)` method:
- Checks if the heartbeat file exists before attempting removal
- Uses `std::fs::remove_file` to delete the file
- Returns `Ok(())` if the file doesn't exist (idempotent)
- Returns an error with descriptive context if removal fails
- Logs debug messages for troubleshooting

### ✅ 2. Use std::fs::remove_file to delete the file
**Location**: `src/health/mod.rs:295`

```rust
std::fs::remove_file(&path).with_context(|| {
    format!(
        "failed to remove heartbeat file: {}",
        path.display()
    )
})?;
```

### ✅ 3. Verify the function works when the heartbeat file exists
**Test**: `healthmonitor_cleanup_heartbeat_file_removes_existing_file` (line 2491)
- Creates a heartbeat file by starting the emitter
- Calls `cleanup_heartbeat_file()`
- Verifies the file is removed

### ✅ 4. Add test to verify file is removed
**Tests**: 8 comprehensive tests cover all scenarios:
- `cleanup_heartbeat_file_removes_existing_file` (line 2065)
- `cleanup_heartbeat_file_ok_when_file_missing` (line 2080)
- `cleanup_heartbeat_file_propagates_errors` (line 2096)
- `cleanup_heartbeat_file_with_heartbeat_path` (line 2121)
- `healthmonitor_cleanup_heartbeat_file_removes_existing_file` (line 2491)
- `healthmonitor_cleanup_heartbeat_file_ok_when_file_missing` (line 2521)
- `healthmonitor_cleanup_heartbeat_file_propagates_errors` (line 2548)
- `healthmonitor_cleanup_heartbeat_file_with_running_emitter` (line 2584)

All tests pass successfully (8/8 passed).

## Related Beads

- `bf-noet` - Added `heartbeat_path` field to HealthMonitor (dependency satisfied)
- `bf-34pp` - Added `std::fs::remove_file` call
- `bf-63o2` - Implemented cleanup function
- `bf-3oy8` - Verified implementation

## Implementation Details

The cleanup function is integrated into the shutdown flow:
1. Called by `HealthMonitor::stop()` (line 324) on graceful shutdown
2. Called by `HealthMonitor::drop()` (line 823) as a fallback
3. Standalone function available for external use (line 856)

The heartbeat file path is computed during construction and stored in the `heartbeat_path` field, making it accessible to the cleanup handler throughout the monitor lifecycle.
