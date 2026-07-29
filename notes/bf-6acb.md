# Bead bf-6acb: Heartbeat File Cleanup Implementation Status

## Task: Implement heartbeat file cleanup on shutdown

## Status: ✅ ALREADY IMPLEMENTED

The heartbeat file cleanup functionality is already fully implemented in the NEEDLE codebase.

## Verification of Acceptance Criteria

### 1. ✅ Heartbeat file path accessible to shutdown handler
- `HealthMonitor::heartbeat_path()` method provides public access
- Path computed during construction: `src/health/mod.rs:376-378`
- Stored in `self.heartbeat_path` for efficient access

### 2. ✅ Cleanup uses std::fs::remove_file
- Implementation in `src/health/mod.rs:327`
- Uses `std::fs::remove_file(&path)` for file removal
- Both instance method and standalone function available

### 3. ✅ Proper error handling (logged, doesn't panic)
- Errors logged with `tracing::warn!` at `src/health/mod.rs:336-340`
- Returns `Ok(())` even on failure to prevent blocking shutdown
- Idempotent: returns Ok for non-existent files

### 4. ✅ Cleanup called from shutdown signal handler
Complete signal flow:
1. Signal handler sets flag (`src/worker/mod.rs:81-94`)
2. Main loop checks flag and calls `self.stop(&reason).await` (`src/worker/mod.rs:2076-2103`)
3. `stop()` method calls `self.health.stop()` (`src/worker/mod.rs:3195`)
4. `health.stop()` calls `cleanup_heartbeat_file()` (`src/health/mod.rs:361`)

## Test Coverage

Comprehensive test suite validates the implementation:
- `heartbeat_cleanup_on_graceful_shutdown` - tests normal shutdown flow
- `heartbeat_cleanup_on_worker_drop` - tests Drop trait fallback
- `cleanup_heartbeat_file_removes_existing_file` - tests file removal
- `cleanup_heartbeat_file_ok_when_file_missing` - tests idempotent behavior
- `cleanup_heartbeat_file_errs_on_removal_failure` - tests error propagation
- `cleanup_heartbeat_file_with_heartbeat_path` - tests actual path format

## Conclusion

No implementation work is required. The acceptance criteria for bead bf-6acb are fully satisfied by the existing codebase. The heartbeat file cleanup is properly integrated with the signal handler and works correctly in all shutdown scenarios.
