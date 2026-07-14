# Test State Analysis for NEEDLE

## Summary

**Current State:** Tests are failing to compile due to breaking API changes in Strand constructors.

**Issue:** All Strand types now require a `Telemetry` parameter in their `::new()` constructors, but test code has not been updated to provide this parameter.

## Compilation Errors

### Immediate Failure (2 errors)
```
error[E0061]: this function takes 2 arguments but 1 argument was supplied
   --> tests/integration_tests.rs:821:17
   |
821 |     let pluck = PluckStrand::new(vec![]);
    |                 ^^^^^^^^^^^^^^^^-------- argument #2 of type `Telemetry` is missing

error[E0061]: this function takes 2 arguments but 1 argument was supplied
   --> tests/integration_tests.rs:877:17
   |
877 |     let pluck = PluckStrand::new(vec![]);
    |                 ^^^^^^^^^^^^^^^^-------- argument #2 of type `Telemetry` is missing
```

## Affected Strand Constructors

All Strand constructors now require `Telemetry` as the last parameter:

| Strand Type | Signature |
|-------------|-----------|
| `PluckStrand::new` | `(exclude_labels: Vec<String>, telemetry: Telemetry)` |
| `ExploreStrand::new` | `(config, home_workspace, registry, telemetry)` |
| `MendStrand::new` | `(config, workspace, agent, telemetry)` |
| `WeaveStrand::new` | `(config, workspace, state_dir, agent, telemetry)` |
| `UnravelStrand::new` | `(config, workspace, state_dir, agent, telemetry)` |
| `PulseStrand::new` | `(config, workspace, state_dir, telemetry)` |
| `KnotStrand::new` | `(config, telemetry)` |

## Tests Requiring Updates

### By Test File

| File | Strand Types | Count |
|------|--------------|-------|
| `integration_tests.rs` | PluckStrand (2), ExploreStrand (3), MendStrand (2) | 7 |
| `p2_integration_tests.rs` | MendStrand (5), ExploreStrand (1) | 6 |
| `p3_integration_tests.rs` | WeaveStrand (3), UnravelStrand (2), PulseStrand (3) | 8 |
| `real_br_integration_tests.rs` | ExploreStrand (3), MendStrand (4) | 7 |

**Total: ~28 test functions need telemetry parameters added**

## Existing Test Statistics

- **Unit tests in src/:** 937 tests
- **Integration test files:** 16 files
- **Test files:** `config_cli_tests.rs`, `heartbeat_validation.rs`, `integration_tests.rs`, `otlp_integration.rs`, `p2_integration_tests.rs`, `p3_integration_tests.rs`, `property_tests.rs`, `real_br_integration_tests.rs`, `routing_integration.rs`, `routing_matcher_baseline.rs`, `sigterm_heartbeat_cleanup.rs`, `test_telemetry_write.rs`, `test_telemetry_write_debug.rs`, `p95_correctness.rs`, `workspace_fixtures.rs`, `needle_transform_claude.rs`

## Heartbeat Validation Test Requirements

### File: `tests/heartbeat_validation.rs`

This test suite validates heartbeat file functionality:

1. **Heartbeat file created on startup**
   - Workers create heartbeat file immediately on startup
   - File path: `<workspace>/.needle/state/heartbeats/<worker-id>.json`

2. **File contains required fields**
   - `worker_id`: String - Worker identifier
   - `qualified_id`: String - Fully qualified worker identity
   - `pid`: Number - Process ID
   - `last_heartbeat`: String - RFC3339 timestamp

3. **Periodic refresh**
   - Updates every `heartbeat_interval_secs` (default: 30s, tests use 1-2s)
   - Timestamp is updated in-place

4. **Graceful shutdown cleanup**
   - Tests use `monitor.stop()` for cleanup
   - File should be removed on graceful shutdown

### Shell Validation: `tests/validate_heartbeat.sh`

This is an end-to-end shell script that validates:

1. **Startup behavior**
   - Worker creates heartbeat file on startup
   - File contains: `worker_id`, `qualified_id`, `pid`, `last_heartbeat`, `state`

2. **Periodic refresh**
   - File refreshes every `heartbeat_interval_secs` (5s in test config)
   - Timestamps are fresh (< 10 seconds old)

3. **Graceful shutdown cleanup**
   - SIGTERM should trigger cleanup
   - Heartbeat file removed after graceful shutdown
   - No stale files remain after exit

### File Removal Logic Requirements

**What needs to be exercised:**

1. **SIGTERM handler** removes heartbeat file
2. **No stale files** remain after worker exit
3. **Clean state directory** after shutdown

**Test coverage gap:** The Rust tests use `monitor.stop()` which is the clean shutdown path. The shell script tests SIGTERM explicitly.

## Test Execution

Currently blocked by compilation errors. Once fixed:

```bash
# Run all tests
cargo test --all-targets

# Run specific test suite
cargo test --test heartbeat_validation

# Run shell validation
./tests/validate_heartbeat.sh
```

## Next Steps

1. Add `Telemetry` parameter to all Strand constructor calls in tests
2. Standardize on `needle::telemetry::Telemetry::new("test".to_string())` for tests
3. Verify compilation succeeds
4. Run tests to identify any remaining failures
5. Update this document with runtime test results
