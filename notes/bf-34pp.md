# Bead bf-34pp: Add std::fs::remove_file call to cleanup_heartbeat_file

## Summary

This bead verified that the `cleanup_heartbeat_file` function in `src/health/mod.rs` contains the required `std::fs::remove_file(path)?;` call.

## Implementation

The function at line 640-660 includes:

```rust
pub fn cleanup_heartbeat_file(path: &Path) -> Result<()> {
    // Check if the file exists before attempting removal.
    if !path.exists() {
        tracing::debug!(
            path = %path.display(),
            "heartbeat file does not exist, skipping cleanup"
        );
        return Ok(());
    }

    // Attempt to remove the file.
    std::fs::remove_file(path)?;

    tracing::debug!(
        path = %path.display(),
        "heartbeat file removed successfully"
    );

    Ok(())
}
```

## Acceptance Criteria Met

- ✓ Function contains a call to `std::fs::remove_file` with the provided `path` argument
- ✓ Call is syntactically correct (matches function signature `path: &Path`)
- ✓ Raw call with `?` for error propagation (no complex error handling yet)

## Tests

All 4 `cleanup_heartbeat_file` tests pass:
- `cleanup_heartbeat_file_removes_existing_file`
- `cleanup_heartbeat_file_ok_when_file_missing`
- `cleanup_heartbeat_file_propagates_errors`
- `cleanup_heartbeat_file_with_heartbeat_path`

## Note

The implementation was completed in a prior commit. This bead verified the existing implementation meets the specified acceptance criteria.
