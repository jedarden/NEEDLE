# Signal Handling Infrastructure Verification

## Bead: bf-45nb

**Title:** Add graceful shutdown signal handler to workers

**Status:** ✅ COMPLETE - Infrastructure already implemented

## Implementation Summary

### 1. Signal Handler Installation ✅

**Location:** `src/worker/mod.rs:920-959`

The `install_signal_handlers()` method is called during worker initialization in `run_inner()` at line 584.

**Components:**
- **Global shutdown flag infrastructure** (lines 46-93)
  - `GLOBAL_SHUTDOWN_FLAG`: AtomicUsize storing pointer to shutdown flag
  - `LAST_SIGNAL`: AtomicU32 tracking last received signal
  - `set_global_shutdown_flag()`: Registers shutdown flag pointer
  - `clear_global_shutdown_flag()`: Cleans up on worker drop
  
- **Signal handler function** (lines 79-93)
  - `signal_handler()`: Synchronous C-compatible signal handler
  - Sets atomic shutdown flag via stored pointer
  - Records signal number for diagnostic logging
  - Async-signal-safe (no allocation, locking, or I/O)

- **Unix signal handler installation** (lines 101-128)
  - `install_unix_signal_handlers()`: Uses libc::sigaction
  - Installs handlers for SIGTERM, SIGINT, SIGHUP
  - Uses SA_RESTART to restart interrupted system calls
  - Blocks all signals during handler execution

### 2. Graceful Shutdown Sequence ✅

**Location:** `src/worker/mod.rs:600-653`

**Flow:**
1. Signal handler sets `shutdown` atomic flag
2. Main loop checks flag between state transitions
3. If set, worker releases current bead (if any)
4. Calls `stop()` to:
   - Emit `worker.stopped` telemetry event
   - Stop heartbeat emitter
   - Deregister from worker registry
   - Flush telemetry buffers

**State-specific handling:**
- **Building/Dispatching/Executing/Handling:** Release bead before stopping
- **Selecting/Claiming/Retrying/Logging:** Stop immediately (no bead held)
- **Booting:** Stop with boot-specific message
- **Stopped/Exhausted/Errored:** Already terminal, just ensure cleanup

### 3. Signal Handling Framework ✅

**Key Components:**

1. **Shared Shutdown Flag**
   - `Arc<AtomicBool> shutdown` field on Worker
   - Shared with HealthMonitor for circuit breaker integration
   - Single source of truth for shutdown state

2. **Atexit Handler** (lines 131-218)
   - Provides last-resort telemetry on unexpected termination
   - Emits `worker.stopped` with diagnostic information
   - Helps distinguish SIGKILL/OOM from graceful shutdown

3. **Platform Support**
   - Unix: Full signal handling via libc
   - Non-Unix: Tokio ctrl_c handler fallback

4. **Safety Measures**
   - Leak of Arc<AtomicBool> to ensure global pointer remains valid
   - Clear global flag on worker drop to avoid dangling pointers
   - Async-signal-safe handler implementation

## Test Coverage

**Location:** `tests/sigterm_heartbeat_cleanup.rs`

**Tests:**
1. `sigterm_removes_heartbeat_file` - Tests signal handling path
2. `drop_trait_cleans_up_heartbeat` - Tests Drop trait cleanup fallback
3. `stop_is_idempotent` - Tests multiple shutdown calls
4. `stop_works_when_emitter_already_exited` - Tests edge cases

## Acceptance Criteria Verification

✅ **Worker installs SIGTERM handler during initialization**
- `install_signal_handlers()` called in `run_inner()` after `boot()` (line 584)
- Synchronous handlers for SIGTERM, SIGINT, SIGHUP via libc::sigaction
- Global shutdown flag pointer registered for handler access

✅ **Handler sets shutdown flag and begins graceful shutdown sequence**
- `signal_handler()` sets atomic shutdown flag (lines 79-93)
- Main loop checks flag and initiates graceful shutdown (lines 600-653)
- Sequence: release bead → stop() → telemetry → deregister

✅ **Signal handling framework is in place**
- Arc<AtomicBool> shutdown flag shared across components
- atexit handler for unexpected termination diagnostics
- Watchdog thread for HANDLING state timeout recovery
- Platform-specific implementations (Unix vs non-Unix)

## Related Components

- **Health Monitor:** Integrates with shutdown flag for circuit breaker
- **Registry:** Deregisters worker on shutdown
- **Telemetry:** Emits worker.stopped event with diagnostics
- **Watchdog:** Independent thread for HANDLING state timeout detection

## Notes

This implementation provides robust signal handling with:
- Immediate response via synchronous handlers
- Graceful shutdown with bead release
- Comprehensive telemetry for debugging
- Protection against wedged HANDLING state
- Cross-platform compatibility

Infrastructure is complete and ready for heartbeat cleanup integration (dependent bead).
