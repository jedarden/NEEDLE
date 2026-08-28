# Log Level Verification for Error Paths

## Summary

This document describes the log level verification infrastructure added to NEEDLE to ensure error cases emit logs at appropriate severity levels.

## Implementation

### Enhanced Log Capture Helper

The `log_capture_helper` module was enhanced with new functions for verifying log levels:

- **`assert_log_level(logs, level)`** - Assert that a specific log level appears in logs
- **`assert_log_level_with_message(logs, level, message)`** - Assert that a log entry exists with both correct level and message
- **`assert_no_error_logs(logs)`** - Assert that no ERROR logs appear
- **`assert_no_warn_logs(logs)`** - Assert that no WARN logs appear
- **`count_log_level(logs, level)`** - Count occurrences of a specific log level
- **`assert_log_level_count(logs, level, expected_count)`** - Assert precise count of log level occurrences

These functions enable precise verification that error conditions emit the correct log severity.

### New Test Suite: `log_level_verification.rs`

Comprehensive test suite covering all log levels:

#### ERROR-Level Tests
- **`error_level_on_heartbeat_file_write_failure`** - Verifies ERROR logs on write permission failures
  - **Expected severity**: ERROR
  - **Rationale**: Write failures prevent heartbeat creation, which is a critical operational failure that prevents worker function

- **`error_level_on_cleanup_failure_after_signal`** - Verifies ERROR logs on cleanup failures after signal reception
  - **Expected severity**: ERROR
  - **Rationale**: Cleanup failure leaves stale heartbeat files, causing false positives in health monitoring

- **`error_level_on_corrupted_heartbeat_file`** - Verifies ERROR logs when encountering corrupted heartbeat files
  - **Expected severity**: ERROR
  - **Rationale**: Corrupted files represent data integrity failures that prevent monitoring system from functioning

#### WARN-Level Tests
- **`warn_level_on_recoverable_heartbeat_refresh_delay`** - Verifies WARN logs on delayed heartbeat refresh
  - **Expected severity**: WARN (if delay exceeds threshold)
  - **Rationale**: Delayed refresh is recoverable but indicates potential performance issues worth flagging

- **`warn_level_on_stale_heartbeat_detection`** - Verifies WARN logs when stale heartbeats are detected
  - **Expected severity**: WARN
  - **Rationale**: Stale heartbeat detection is a warning condition, not a system failure

#### DEBUG-Level Tests
- **`debug_level_on_heartbeat_state_inspection`** - Verifies DEBUG logs during state inspection operations
  - **Expected severity**: DEBUG
  - **Rationale**: State inspection is diagnostic information useful for troubleshooting but not actionable in production

- **`debug_level_on_detailed_worker_state_transition`** - Verifies DEBUG logs during state transitions
  - **Expected severity**: DEBUG
  - **Rationale**: Detailed state transitions provide verbose diagnostic information for development/troubleshooting

#### INFO-Level Tests
- **`info_level_on_successful_heartbeat_operations`** - Verifies INFO logs for successful heartbeat operations
  - **Expected severity**: INFO
  - **Rationale**: Successful operations are normal events that should be logged for operational visibility

### Enhanced Existing Tests

The existing heartbeat test suites were enhanced with log level verification:

- **`sigterm_heartbeat_cleanup.rs`** - Added log level assertions for cleanup operations
- **`heartbeat_validation.rs`** - Added log level assertions for validation operations

Each test now includes comments explaining the expected log severity and rationale.

## Log Severity Guidelines

### ERROR Level
- Use for: Actual failures that prevent operation completion
- Examples: Write permission errors, cleanup failures, data corruption
- Rationale: These are critical operational failures requiring immediate attention

### WARN Level
- Use for: Recoverable issues that don't prevent operation completion
- Examples: Delayed operations, stale data detection, performance issues
- Rationale: Notable but not broken - worth monitoring but not emergencies

### DEBUG Level
- Use for: Diagnostic information for troubleshooting
- Examples: State transitions, detailed inspection, verbose tracing
- Rationale: Useful for development and troubleshooting but too verbose for production

### INFO Level
- Use for: Normal operational messages
- Examples: Successful operations, normal state changes, lifecycle events
- Rationale: Confirms system is operating correctly without indicating problems

## Test Results

All log level verification tests pass, confirming that:
1. ERROR logs are emitted on actual failures
2. WARN logs are emitted on recoverable issues
3. DEBUG logs are emitted on diagnostic paths
4. INFO logs are emitted for normal operations
5. Each error simulation produces the correct log level

## Usage

To verify log levels in new tests:

```rust
let (logs, _guard) = log_capture_helper::setup_log_capture();

// Run code that should emit ERROR logs
some_operation_that_might_fail();

// Verify ERROR was emitted
log_capture_helper::assert_log_level_with_message(&logs, "ERROR", "operation failed");

// Verify no ERROR was emitted (for successful operations)
log_capture_helper::assert_no_error_logs(&logs);
```

## Dependencies

- Depends on bead `needle-284f2bb1` (log capture infrastructure)
- Part of the logging verification chain
- Built on top of existing `log_capture_helper` module

## Files Modified

1. `tests/log_capture_helper.rs` - Enhanced with log level verification functions
2. `tests/log_level_verification.rs` - New comprehensive test suite
3. `tests/sigterm_heartbeat_cleanup.rs` - Added log level verification
4. `tests/heartbeat_validation.rs` - Added log level verification
5. `tests/routing_telemetry_verification.rs` - Fixed compilation error (extra closing brace)

## Acceptance Criteria Met

✅ Add assertions for ERROR-level logs on actual failures
✅ Add assertions for WARN-level logs on recoverable issues
✅ Add assertions for DEBUG-level logs on diagnostic paths
✅ Verify each error simulation produces correct log level
✅ Add test comments explaining expected log severity
