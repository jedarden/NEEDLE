# Retry Test Infrastructure Guide

This guide explains how to use the retry test infrastructure in NEEDLE for testing retry logic, error injection, and backoff strategies.

## Overview

The retry test infrastructure provides:

- **Test helpers** (`tests/retry_test_helpers.rs`) - Reusable utilities for retry testing
- **Mock framework** - Configurable error injection for simulating failure scenarios
- **Test fixtures** - Pre-configured retry patterns and assertion helpers
- **No panics** - All functions return `Result` to avoid `expect()` and `unwrap()` in tests

## Quick Start

```rust
use retry_test_helpers::*;

#[test]
fn test_etxtbsy_retry() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(3)
        .with_etxtbsy_on_attempt(1)
        .with_backoff_ms(20);

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 2)?;
    Ok(())
}
```

## Core Components

### 1. Error Injection

The `ErrorInjection` type lets you specify which errors occur on which attempts:

```rust
let injection = ErrorInjection::new()
    .with_etxtbsy_on_attempt(1)      // ETXTBSY on first attempt
    .with_io_error_on_attempt(2, io::ErrorKind::NotFound, "not found"); // NotFound on second
```

**Available methods:**
- `with_error_on_attempt(attempt, error)` - Inject any error
- `with_etxtbsy_on_attempt(attempt)` - Inject ETXTBSY error (errno 26)
- `with_io_error_on_attempt(attempt, kind, msg)` - Inject IO error

### 2. Retry Configuration

The `RetryConfig` type defines retry behavior:

```rust
let config = RetryConfig::new()
    .with_max_attempts(5)
    .with_backoff_ms(20)
    .with_exponential_backoff(10, 1000); // Initial 10ms, max 1000ms
```

**Configuration options:**
- `max_attempts` - Maximum retry attempts (default: 5)
- `backoff_ms` - Linear backoff delay (default: 20ms)
- `exponential_backoff` - Enable exponential backoff (default: false)
- `exponential_initial_ms` - Initial exponential delay (default: 10ms)
- `exponential_max_ms` - Maximum exponential delay (default: 1000ms)

### 3. Mock Retry Behavior

The `MockRetryBehavior` executor runs simulated retry scenarios:

```rust
let result = MockRetryBehavior::new()
    .with_max_attempts(3)
    .with_etxtbsy_on_attempt(1)
    .run_sync()?;
```

**Available methods:**
- `with_max_attempts(max)` - Set max attempts
- `with_backoff_ms(ms)` - Set backoff delay
- `with_exponential_backoff(initial, max)` - Enable exponential backoff
- `with_error_on_attempt(attempt, error)` - Inject error on attempt
- `with_etxtbsy_on_attempt(attempt)` - Inject ETXTBSY on attempt
- `with_success_on_attempts(attempts)` - Specify which attempts succeed
- `with_success_value(value)` - Set success return value
- `run_sync()` - Run synchronously
- `run_async()` - Run asynchronously

## Result Structure

Every retry operation returns a `RetryResult`:

```rust
pub struct RetryResult {
    pub attempts: usize,           // Number of attempts made
    pub succeeded: bool,           // Whether operation succeeded
    pub elapsed: Duration,         // Total elapsed time
    pub result: Option<Vec<u8>>,   // Success value (if succeeded)
    pub error: Option<io::Error>, // Final error (if failed)
}
```

## Assertion Helpers

Use assertion helpers to validate results safely:

```rust
// Assert success with specific attempt count
assert_succeeded_with_attempts(&result, 2)?;

// Assert failure with specific error kind
assert_failed_with_error_kind(&result, io::ErrorKind::NotFound)?;

// Assert ETXTBSY failure
assert_failed_etxtbsy(&result)?;

// Assert retry stayed within bounds
assert_retry_within_bounds(&result, 5)?;
```

## Common Patterns

### Pattern 1: ETXTBSY Retry

Test retry behavior for ETXTBSY (errno 26) errors:

```rust
#[test]
fn test_etxtbsy_retry_succeeds() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(3)
        .with_etxtbsy_on_attempt(1)
        .with_backoff_ms(20);

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 2)?;
    assert!(result.elapsed.as_millis() >= 20); // At least one backoff
    Ok(())
}
```

### Pattern 2: Non-Retryable Errors

Test that non-retryable errors fail immediately:

```rust
#[test]
fn test_non_retryable_error_fails_immediately() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(5)
        .with_io_error_on_attempt(1, io::ErrorKind::NotFound, "not found");

    let result = mock.run_sync()?;

    assert_failed_with_error_kind(&result, io::ErrorKind::NotFound)?;
    assert_eq!(result.attempts, 1); // Should not retry
    Ok(())
}
```

### Pattern 3: Exponential Backoff

Test exponential backoff behavior:

```rust
#[test]
fn test_exponential_backoff() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(4)
        .with_etxtbsy_on_attempt(1)
        .with_etxtbsy_on_attempt(2)
        .with_exponential_backoff(10, 100);

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 3)?;
    // 10ms + 20ms = at least 30ms total backoff
    assert!(result.elapsed.as_millis() >= 30);
    Ok(())
}
```

### Pattern 4: Multiple Retries

Test behavior after multiple retries:

```rust
#[test]
fn test_multiple_retries_success() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(5)
        .with_etxtbsy_on_attempt(1)
        .with_etxtbsy_on_attempt(2)
        .with_etxtbsy_on_attempt(3)
        .with_backoff_ms(50);

    let result = mock.run_sync()?;

    assert_succeeded_with_attempts(&result, 4)?;
    assert!(result.elapsed.as_millis() >= 150); // 3 * 50ms backoff
    Ok(())
}
```

### Pattern 5: Async Retry Testing

Test async retry behavior:

```rust
#[tokio::test]
async fn test_async_retry() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(3)
        .with_etxtbsy_on_attempt(1)
        .with_backoff_ms(50);

    let result = mock.run_async().await?;

    assert_succeeded_with_attempts(&result, 2)?;
    assert!(result.elapsed.as_millis() >= 50);
    Ok(())
}
```

## Error Types

### ETXTBSY (errno 26)

The most common retry scenario - binary is temporarily busy:

```rust
let error = etxtbsy_error();
assert_eq!(error.raw_os_error(), Some(26));
```

### IO Errors

Create custom IO errors:

```rust
let error = io_error(io::ErrorKind::NotFound, "file not found");
assert_eq!(error.kind(), io::ErrorKind::NotFound);
```

## Integration with Existing Tests

The retry infrastructure integrates with existing NEEDLE retry logic in:

- `src/util.rs` - `parse_backend_name_from_version()` with ETXTBSY retry
- `tests/etxtbsy_retry.rs` - Existing ETXTBSY retry tests
- `src/bead_store/` - CLI operation retry strategies

Example integration:

```rust
use needle::util::parse_backend_name_from_version;
use retry_test_helpers::*;

#[test]
fn test_util_retry_with_helper() -> Result<(), String> {
    let mock = MockRetryBehavior::new()
        .with_max_attempts(5)
        .with_etxtbsy_on_attempt(1)
        .with_backoff_ms(20);

    let result = mock.run_sync()?;

    // Verify retry behavior matches util.rs implementation
    assert_succeeded_with_attempts(&result, 2)?;
    assert!(result.elapsed.as_millis() >= 20);

    Ok(())
}
```

## Best Practices

### 1. Always Return Results

Never use `expect()` or `unwrap()` in test code:

```rust
// BAD
let result = mock.run_sync().unwrap();

// GOOD
let result = mock.run_sync()?;
```

### 2. Use Specific Assertions

Prefer specific assertions over generic ones:

```rust
// BAD
assert!(result.succeeded);

// GOOD
assert_succeeded_with_attempts(&result, 2)?;
```

### 3. Test Timing Constraints

Verify backoff timing for retry tests:

```rust
assert!(result.elapsed.as_millis() >= expected_backoff);
```

### 4. Cover Edge Cases

Test boundary conditions:

```rust
// Single attempt, no retry
let mock = MockRetryBehavior::new()
    .with_max_attempts(1)
    .with_etxtbsy_on_attempt(1);

// Large max attempts
let mock = MockRetryBehavior::new()
    .with_max_attempts(100)
    .with_success_on_attempts(vec![50]);
```

## Examples

See `tests/retry_infrastructure_examples.rs` for comprehensive examples including:

- Basic retry patterns
- Error injection scenarios
- Exponential backoff testing
- Async retry testing
- Edge cases and boundary conditions
- Real-world pattern simulation

## Testing the Infrastructure Itself

The retry test infrastructure includes self-tests:

```bash
cargo test retry_test_helpers
cargo test retry_infrastructure_examples
```

## Module Structure

```
tests/
├── retry_test_helpers.rs              # Core test helpers and utilities
├── retry_infrastructure_examples.rs   # Example tests demonstrating usage
└── etxtbsy_retry.rs                   # Existing ETXTBSY retry tests

docs/
└── retry-test-infrastructure-guide.md # This guide

src/
├── util.rs                            # Production retry logic
└── bead_store/                        # CLI retry strategies
```

## Future Enhancements

Potential improvements to the retry test infrastructure:

1. **Jitter support** - Add random jitter to backoff delays
2. **Custom retry predicates** - Allow custom retry logic beyond ETXTBSY
3. **Metrics collection** - Track retry statistics across tests
4. **Flaky test detection** - Identify tests with inconsistent retry behavior
5. **Stress testing** - High-volume retry scenarios with many failures

## Contributing

When adding new retry tests:

1. Use `Result<(), String>` return type
2. Avoid `expect()` and `unwrap()`
3. Use assertion helpers from `retry_test_helpers`
4. Add examples to `retry_infrastructure_examples.rs`
5. Update this guide with new patterns

## Troubleshooting

### Issue: Tests fail inconsistently

**Solution:** Ensure proper isolation and avoid shared state. Use `#[tokio::test]` for async tests.

### Issue: Backoff timing varies

**Solution:** Timing checks use `>=` to account for system load. Avoid exact timing assertions.

### Issue: Retry attempts exceed max_attempts

**Solution:** Verify error injection configuration. Non-ETXTBSY errors don't retry by default.

## References

- ADR-013: Bead Backend Operation Strategies
- CLAUDE.md: Testing conventions and isolation requirements
- `src/util.rs`: ETXTBSY retry implementation
- `tests/etxtbsy_retry.rs`: Existing retry test patterns
