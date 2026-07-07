# Bead bf-3syk: Expose heartbeat_path getter for shutdown handler

## Summary

Verified that the `heartbeat_path()` getter method already exists in the `HealthMonitor` struct and is working correctly.

## Existing Implementation

The public getter method is defined at `src/health/mod.rs:336-345`:

```rust
pub fn heartbeat_path(&self) -> PathBuf {
    self.heartbeat_path.clone()
}
```

## Acceptance Criteria Verification

- ✅ **Public getter method exists**: `pub fn heartbeat_path(&self) -> PathBuf`
- ✅ **Returns the path**: Returns an owned `PathBuf` (not `Option<&PathBuf>` since the path is always set during construction)
- ✅ **Path is always set**: The path is computed during construction in `new()` at line 139
- ✅ **Used by shutdown handler**: The `stop()` method calls `cleanup_heartbeat_file()` which uses `self.heartbeat_path()`
- ✅ **Code quality verified**: `cargo clippy --all-targets -- -D warnings` passes with no warnings
- ✅ **Tests pass**: The `heartbeat_path_uses_qualified_id_not_bead_worker_id` test confirms the method works correctly

## Usage

The method is used throughout the codebase:
- Line 206: Error context formatting
- Line 240: Logging in `start_emitter()`
- Line 281: File cleanup in `cleanup_heartbeat_file()`
- Line 368: Verification in `verify_heartbeat()`
- Line 795: Writing the heartbeat file in `write_heartbeat()`

## Conclusion

The bead's requirements are already satisfied by the existing implementation. No code changes were needed.
