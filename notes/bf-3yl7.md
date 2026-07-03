# Bead bf-3yl7: Add cleanup_heartbeat_file function stub

## Finding

The `cleanup_heartbeat_file` function **already exists** in the codebase.

## Location

File: `/home/coding/NEEDLE/src/health/mod.rs`
Lines: 615-661
Implemented in: commit `ee316fb` (for bead bf-63o2)

## Acceptance Criteria - All Met ✓

1. **Function signature**: `cleanup_heartbeat_file(path: &Path) -> Result<()>` ✓
   - Exact match at line 640

2. **Correct module placement**: ✓
   - Located in `src/health/mod.rs` (appropriate for heartbeat-related functionality)
   - Not in strand.rs as initially speculated, but in the health module which is more appropriate

3. **Function implementation**: ✓ (exceeds stub requirement)
   - Complete implementation with proper error handling
   - Uses `std::fs::remove_file` for deletion
   - Returns `Ok(())` if file is removed or doesn't exist
   - Returns `Err` with context if removal fails

4. **Compiles without errors**: ✓
   - Code compiles successfully
   - Has comprehensive test coverage (4 tests covering all acceptance criteria)

## Implementation Details

The function includes:
- File existence check before removal
- Proper error handling with `anyhow::Context`
- Debug tracing logs for operations
- Full rustdoc documentation with examples
- 4 comprehensive tests:
  - `cleanup_heartbeat_file_removes_existing_file`
  - `cleanup_heartbeat_file_ok_when_file_missing`
  - `cleanup_heartbeat_file_propagates_errors`
  - `cleanup_heartbeat_file_with_heartbeat_path`

## Conclusion

No code changes needed. The task requirements are already satisfied by the existing implementation, which is production-ready with proper error handling, logging, documentation, and test coverage.
