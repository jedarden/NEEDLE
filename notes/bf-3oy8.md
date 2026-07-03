# Bead bf-3oy8: Add std::fs::remove_file call to cleanup_heartbeat_file

## Task Verification

Verified that `cleanup_heartbeat_file` in `src/health/mod.rs` (line 640-660) already contains the required `std::fs::remove_file(path)?;` call.

## Acceptance Criteria Met

All criteria verified as present in the existing implementation:

1. ✅ Function contains `std::fs::remove_file(&path)` call (line 652)
2. ✅ Call uses the provided `path` argument
3. ✅ Raw `std::fs::remove_file` call is present
4. ✅ Syntax matches function signature (`path: &Path`)

## Implementation Details

The function:
- Checks if the file exists before attempting removal
- Calls `std::fs::remove_file(path)?` to remove the file
- Returns `Ok(())` for non-existent files (no-op behavior)
- Logs debug messages for both skip and success cases

The implementation is complete and correct.
