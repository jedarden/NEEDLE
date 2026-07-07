# Bead bf-4afn: Add cleanup_heartbeat_file function signature

## Task
Add the function signature for cleanup_heartbeat_file.

## Acceptance Criteria Verification

### ✓ Function signature: `cleanup_heartbeat_file(path: &Path) -> Result<()>`
Verified at line 860 in `src/health/mod.rs`:
```rust
pub fn cleanup_heartbeat_file(path: &Path) -> Result<()>
```

### ✓ Function returns Ok(()) as a placeholder
The function returns `Ok(())` in all code paths (lines 868, 889). Errors are logged but not returned, making cleanup best-effort.

### ✓ Function is properly documented with a doc comment
Lines 834-859 contain comprehensive doc comments including:
- Purpose description
- Arguments documentation
- Returns documentation with error conditions
- Example usage code

### ✓ Function is placed in the appropriate module
The function is located in `src/health/mod.rs` in the "Utility functions" section (lines 834-890), which is the appropriate location for heartbeat-related cleanup functionality.

## Additional Notes

The function was already implemented in a previous commit (5c1560f: "style(needle-bf-4afn): format code and fix clippy warnings"). The implementation includes:

1. Idempotent cleanup (returns Ok if file doesn't exist)
2. Error logging without failure (best-effort cleanup)
3. Proper tracing for observability
4. Comprehensive test coverage (4 test functions)

## Test Coverage

The function has 4 dedicated test functions:
- `cleanup_heartbeat_file_removes_existing_file`
- `cleanup_heartbeat_file_ok_when_file_missing`
- `cleanup_heartbeat_file_logs_errors_on_failure`
- `cleanup_heartbeat_file_with_heartbeat_path`

All acceptance criteria are met.
