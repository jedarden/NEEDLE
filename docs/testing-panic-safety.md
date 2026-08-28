# Panic Safety Guarantees

This document describes the panic safety guarantees provided by the NEEDLE codebase and how they are verified through automated testing.

## Overview

NEEDLE is designed to handle all error conditions gracefully without unwinding panics. All error paths in production code return `Result` types rather than calling `panic!`, `unwrap()`, or `expect()`.

## Core Safety Guarantees

### 1. No Unwinding Panics on Errors

**Guarantee**: All error conditions return `Err(Result)` instead of panicking.

**Implementation**:
- All functions that can fail return `Result<T, E>`
- Error propagation uses the `?` operator
- No `unwrap()`, `expect()`, or `panic!()` in production code paths
- Malicious or malformed input causes graceful error returns

**Test Coverage**: See `tests/panic_safety_verification.rs`:
- `predispatch_record_handles_invalid_workspace`
- `commit_hook_injection_handles_nonexistent_workspace`
- `resolver_handles_invalid_json_without_panic`
- `validation_gate_handles_command_failure_without_panic`

### 2. Double Cleanup Safety

**Guarantee**: Cleanup functions are idempotent and safe to call multiple times.

**Implementation**:
- Cleanup operations use `std::fs::remove_file` with error suppression
- Multiple cleanup calls on the same resource are safe
- Cleanup functions return `()` instead of propagating IO errors

**Test Coverage**: See `tests/panic_safety_verification.rs`:
- `predispatch_clear_is_idempotent`
- `predispatch_clear_handles_nonexistent_path`
- `cleanup_happens_on_error_paths`
- `error_in_cleanup_does_not_cause_panic`

### 3. Graceful Degradation

**Guarantee**: System continues operating under degraded conditions.

**Implementation**:
- Predispatch snapshot failures are logged but don't block dispatch
- Validation gate failures return structured results rather than panicking
- Resolver failures return safe fallback decisions

**Test Coverage**: See `tests/panic_safety_verification.rs`:
- `predispatch_load_handles_malformed_snapshot`
- `commit_hook_validation_handles_malformed_predispatch`
- `resolver_handles_timeout_without_panic`

### 4. Resource Cleanup on Error

**Guarantee**: All resources are properly cleaned up even when operations fail.

**Implementation**:
- Temporary files are cleaned up via RAII guards
- File locks are released on drop
- All cleanup happens in `Drop` implementations or explicit cleanup functions

**Test Coverage**: See `tests/panic_safety_verification.rs`:
- `cleanup_happens_on_error_paths`
- `error_in_cleanup_does_not_cause_panic`

### 5. Timeout Resilience

**Guarantee**: Long-running operations timeout safely without panicking.

**Implementation**:
- All subprocess calls use `tokio::time::timeout`
- Timeouts return `Err` instead of panicking
- Processes are killed with `kill_on_drop(true)`

**Test Coverage**: See `tests/panic_safety_verification.rs`:
- `resolver_handles_timeout_without_panic`
- `validation_gate_handles_timeout_without_panic`
- `commit_hook_injection_handles_pushed_commit`

### 6. Concurrent Operation Safety

**Guarantee**: Concurrent cleanup operations don't cause data races or panics.

**Implementation**:
- Snapshot operations use atomic file writes
- Flock-based serialization for commit operations
- No mutable static state

**Test Coverage**: See `tests/panic_safety_verification.rs`:
- `predispatch_clear_concurrent_safe`
- `concurrent_snapshot_operations_are_safe`
- `concurrent_validation_gates_are_safe`

## Testing Strategy

### Test Categories

1. **Error Path Tests**: Verify all error cases return `Result` without panicking
2. **Double Cleanup Tests**: Verify cleanup functions are idempotent
3. **Resource Exhaustion Tests**: Verify graceful handling of resource limits
4. **Timeout Tests**: Verify operations timeout safely
5. **Concurrent Cleanup Tests**: Verify thread-safe cleanup operations

### Running Panic Safety Tests

```bash
# Run all panic safety verification tests
cargo test --test panic_safety_verification

# Run specific test categories
cargo test --test panic_safety_verification predispatch
cargo test --test panic_safety_verification commit_hook
cargo test --test panic_safety_verification resolver
cargo test --test panic_safety_verification concurrent
```

### Test Isolation

All panic safety tests use proper isolation to avoid interfering with each other:

- Test repositories are created in temporary directories via `tempfile::TempDir`
- Each test uses unique bead IDs to avoid conflicts
- Filesystem state is cleaned up on test completion
- Environment variables are restored after test execution

## Panic Capture Infrastructure

NEEDLE includes a panic capture module (`src/panic_capture.rs`) that:

1. Installs a custom panic hook that captures full stack traces
2. Ensures `RUST_BACKTRACE=1` is set for complete panic information
3. Formats panic output consistently for parsing
4. Is idempotent - safe to call multiple times

### Installing the Panic Hook

```rust
use needle::panic_capture::install_panic_hook;

fn main() {
    install_panic_hook();
    // ... rest of application
}
```

The panic hook is automatically installed by NEEDLE workers during startup.

## Error Handling Patterns

### Pattern 1: Result Return with Context

```rust
use anyhow::{Context, Result};

pub fn read_config(path: &Path) -> Result<Config> {
    let content = std::fs::read(path)
        .with_context(|| format!("failed to read config from {}", path.display()))?;
    serde_json::from_slice(&content)
        .with_context(|| "failed to parse config JSON")?
}
```

### Pattern 2: Safe Cleanup with Error Suppression

```rust
pub async fn clear_snapshot(workspace: &Path, bead_id: &BeadId) {
    let path = snapshot_path(workspace, bead_id);
    // Silently ignore cleanup errors - resource cleanup should never panic
    let _ = tokio::fs::remove_file(&path).await;
}
```

### Pattern 3: Timeout with Fallback

```rust
use tokio::time::{timeout, Duration};

pub async fn run_with_timeout<T, E>(duration: Duration, operation: impl Future<Output = Result<T, E>>) -> Result<T, E> {
    timeout(duration, operation)
        .await
        .map_err(|_| anyhow::anyhow!("operation timed out after {:?}", duration))?
}
```

### Pattern 4: Idempotent Operations

```rust
pub fn ensure_directory_exists(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}
// Safe to call multiple times - idempotent
```

## Verification in CI

All panic safety tests run automatically in CI on every push to main. The test suite ensures:

1. No new code introduces panic points in error paths
2. Double cleanup operations remain safe
3. Concurrent operations don't introduce data races
4. Timeout handling works correctly

### CI Configuration

Panic safety tests are integrated into the main `cargo test` run and must pass before any merge is accepted.

## Adding New Panic Safety Tests

When adding new functionality, include panic safety tests that:

1. Test the primary error path returns `Result` without panicking
2. Test cleanup is idempotent if applicable
3. Test timeout handling if the operation is long-running
4. Test concurrent operations if applicable
5. Document the specific safety guarantee being tested

### Example Test Template

```rust
#[tokio::test]
async fn my_feature_handles_error_without_panic() {
    // Panic safety guarantee: my_feature() should return Result error
    // instead of panicking when given invalid input.
    
    let invalid_input = "clearly_invalid_input";
    
    // Should return Err without panicking
    let result = my_feature(invalid_input).await;
    
    // Verify error was returned gracefully
    assert!(result.is_err() || result.is_ok());
    // The key assertion is that we reached here without panicking
}
```

## Monitoring Panic Safety

In production, NEEDLE monitors:

1. **Panic Rate**: Number of panics per 1000 operations
2. **Error Rate**: Number of errors vs successful operations
3. **Timeout Rate**: Number of timeouts vs successful operations
4. **Cleanup Success Rate**: Frequency of cleanup failures

Alerts are triggered if:
- Panic rate exceeds 0.1% (zero tolerance for panics)
- Error rate spikes above normal baseline
- Timeout rate exceeds 5%

## References

- Test suite: `tests/panic_safety_verification.rs`
- Panic capture: `src/panic_capture.rs`
- Commit hooks: `src/commit_hook.rs`
- Validation gates: `src/validation/mod.rs`
- Resolver: `src/resolve/mod.rs`
