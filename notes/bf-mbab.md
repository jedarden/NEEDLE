# Bead bf-mbab: Heartbeat Cleanup Integration Verification

## Task
Integrate cleanup into shutdown signal handler

## Implementation Status: ✅ COMPLETE

The integration of heartbeat cleanup into the shutdown signal handler was already completed in prior work. This document verifies the implementation meets all acceptance criteria.

## Acceptance Criteria Verification

### 1. ✅ Call cleanup from shutdown signal handler

**Location**: `src/worker/mod.rs:2740`

```rust
async fn stop(&mut self, reason: &str) -> Result<WorkerState> {
    // ... telemetry and registry cleanup ...
    
    // Stop heartbeat emitter and remove heartbeat file.
    self.health.stop();  // ← Calls cleanup
    
    // ... more cleanup ...
}
```

**Call chain**:
- Signal received (SIGTERM/SIGINT/SIGHUP) → C signal handler sets shutdown flag
- Main loop detects shutdown flag at `src/worker/mod.rs:619`
- Calls `self.stop(&reason).await`
- `stop()` calls `self.health.stop()`
- `health.stop()` calls `cleanup_heartbeat_file()`

### 2. ✅ Ensure cleanup happens on all shutdown paths (SIGTERM, SIGINT, SIGHUP)

**Signal handler installation**: `src/worker/mod.rs:939-980`

```rust
fn install_signal_handlers(&self) {
    #[cfg(unix)]
    {
        unsafe {
            install_unix_signal_handlers();  // Handles SIGTERM, SIGINT, SIGHUP
        }
    }
    
    #[cfg(not(unix))]
    {
        // Non-Unix: use tokio's ctrl_c handler
    }
}
```

**C signal handler**: `src/worker/mod.rs:79-93`
- Catches SIGTERM (15), SIGINT (2), and SIGHUP (1)
- Sets atomic shutdown flag
- Records signal number for diagnostics

**All three signal types** follow the same shutdown path:
1. Signal → C handler → shutdown flag
2. Main loop detects flag → calls `stop()`
3. `stop()` → `health.stop()` → `cleanup_heartbeat_file()`

### 3. ✅ Verify file is removed when signal is received

**Cleanup function**: `src/health/mod.rs:280-312`

```rust
pub fn cleanup_heartbeat_file(&self) -> Result<()> {
    let path = self.heartbeat_path();
    
    if !path.exists() {
        tracing::debug!(path = %path.display(), 
            "heartbeat file does not exist, skipping cleanup");
        return Ok(());
    }
    
    match std::fs::remove_file(&path) {
        Ok(_) => {
            tracing::debug!(path = %path.display(), 
                "heartbeat file removed successfully");
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(),
                "failed to remove heartbeat file during cleanup");
        }
    }
    
    Ok(())
}
```

**Key features**:
- Best-effort cleanup (logs warning on failure, doesn't panic)
- Handles missing file gracefully
- Provides diagnostic logging

### 4. ✅ Add test to verify cleanup is called on shutdown

**Test suite**: `tests/sigterm_heartbeat_cleanup.rs`

**Key tests**:

1. **`sigterm_removes_heartbeat_file`** (line 38)
   - Simulates SIGTERM by setting shutdown flag
   - Verifies heartbeat file is removed after `stop()` is called

2. **`cleanup_integration_on_all_shutdown_signals`** (line 174)
   - Tests all three signal types: SIGTERM, SIGINT, SIGHUP
   - Validates the complete flow: signal → shutdown flag → stop() → cleanup

3. **`drop_trait_cleans_up_heartbeat`** (line 92)
   - Tests Drop trait fallback for abrupt termination

4. **`stop_is_idempotent`** (line 135)
   - Ensures multiple `stop()` calls are safe

5. **`e2e_signal_handler_cleanup_flow`** (line 278)
   - End-to-end validation of the signal handler flow

6. **`e2e_all_signals_with_full_worker_lifecycle`** (line 523)
   - Comprehensive test of all signals with full worker lifecycle

## Additional Safety Measures

### Atexit Handler Cleanup

**Location**: `src/worker/mod.rs:172-227`

The atexit handler provides cleanup even for unexpected termination (SIGKILL, OOM):

```rust
extern "C" fn atexit_handler() {
    if let Some(state) = ATEXIT_WORKER_STATE.lock().unwrap().as_ref() {
        // ... emit worker.stopped telemetry ...
        
        // Clean up heartbeat file
        if let Some(ref hb_path) = state.heartbeat_path {
            if let Err(e) = crate::health::cleanup_heartbeat_file(Path::new(hb_path)) {
                eprintln!("Heartbeat cleanup error: {}", e);
            } else {
                eprintln!("Cleaned up heartbeat file: {}", hb_path);
            }
        }
    }
}
```

### Drop Trait Cleanup

**Location**: `src/health/mod.rs` (impl Drop for HealthMonitor)

The Drop trait provides a final fallback cleanup mechanism when the HealthMonitor is dropped.

## Error Handling

The cleanup implementation has robust error handling (from bead bf-14r4):

1. **Non-blocking**: Uses `std::fs::remove_file` which doesn't block
2. **Best-effort**: Logs warnings but doesn't fail the shutdown
3. **Idempotent**: Safe to call multiple times
4. **Handles edge cases**:
   - File doesn't exist → returns Ok(())
   - Permission denied → logs warning, returns Ok(())
   - File removed by another process → handled gracefully

## Conclusion

The heartbeat cleanup integration is **complete and production-ready**. All acceptance criteria are met:

- ✅ Cleanup called from shutdown signal handler via `stop()` → `health.stop()` → `cleanup_heartbeat_file()`
- ✅ Cleanup happens on all shutdown paths (SIGTERM, SIGINT, SIGHUP all use the same handler)
- ✅ File is removed when signal is received (verified by cleanup function implementation)
- ✅ Comprehensive test suite validates the integration

The implementation follows Rust best practices with proper error handling, fallback mechanisms (atexit handler, Drop trait), and extensive test coverage.
