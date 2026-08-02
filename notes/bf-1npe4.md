# Bead bf-1npe4: stderr.txt File Writing

## Status: Already Implemented

The `write_stderr` functionality was already fully implemented in `src/trace/mod.rs` when this bead was claimed.

## Implementation Details

### Location
- File: `src/trace/mod.rs`
- Lines: 182-203 (implementation)
- Tests: lines 602-642 (successful write and error handling)

### Features Implemented

1. **Stderr File Writing** (`TraceCapture::write_stderr`)
   - Writes to `.beads/traces/<bead-id>/stderr.txt`
   - Returns `Result<()>` for error propagation

2. **Sanitization Integration**
   - Calls `self.sanitize(stderr)` before writing
   - Uses configured `Sanitizer` if present
   - No unsanitized content ever touches disk

3. **Graceful Error Handling**
   - Logs write errors with `tracing::warn!`
   - Includes path and error details in log
   - Returns error with context via `anyhow::Context`

4. **Comprehensive Tests**
   - `trace_capture_writes_stderr`: Tests successful write
   - `trace_capture_write_stderr_handles_errors_gracefully`: Tests error handling with read-only directory

### Test Results

All 22 trace module tests pass:
```
test trace::tests::trace_capture_writes_stderr ... ok
test trace::tests::trace_capture_write_stderr_handles_errors_gracefully ... ok
```

## Code Quality

- Follows the same pattern as `write_stdout`
- No `unwrap()` or `expect()` in production code
- Proper error propagation with `anyhow`
- Exhaustive match arms (no catch-all `_`)
- Fully documented with doc comments

## Notes

This bead was assigned but the work was already complete. The implementation existed prior to bead creation, likely as part of the initial trace capture infrastructure or in a previous related bead (bf-4extd documented stdout.txt implementation).
