# Bead bf-2x4ni: Add output file writing logic

## Status: COMPLETE

## Summary

The required functionality for writing cargo test output to bead trace files was **already fully implemented** in the existing `src/trace/mod.rs` module.

## Acceptance Criteria (All Met)

1. ✅ **Write stdout to file in `.beads/traces/<bead-id>/stdout.txt`**
   - Implemented: `TraceCapture::write_stdout()` (line 147-155)
   - Writes sanitized stdout content to `stdout.txt`

2. ✅ **Write stderr to file in `.beads/traces/<bead-id>/stderr.txt`**
   - Implemented: `TraceCapture::write_stderr()` (line 157-165)
   - Writes sanitized stderr content to `stderr.txt`

3. ✅ **Ensure parent directory exists before writing**
   - Implemented: `TraceCapture::new()` creates directory via `std::fs::create_dir_all()` (line 122)
   - Returns `None` if directory creation fails, with proper error logging

4. ✅ **Handle file I/O errors gracefully**
   - Implemented: All write methods return `Result<()>`
   - Uses `.with_context()` for detailed error messages
   - Log warnings emitted when directory creation fails

## Existing Implementation Details

The `TraceCapture` struct (lines 89-96) provides:
- **Constructor**: `TraceCapture::new(bead_id, workspace_root) -> Option<Self>`
- **Stdout writing**: `write_stdout(&self, stdout: &str) -> Result<()>`
- **Stderr writing**: `write_stderr(&self, stderr: &str) -> Result<()>`
- **Directory access**: `trace_dir(&self) -> &Path`

Additional features beyond requirements:
- Optional sanitization of content before writing
- Support for trace.jsonl structured traces
- Metadata.json writing with timing/cost info
- Prune/delete functionality for retention policies

## Integration

The `cargo_test` module already uses this implementation:
- `run_with_bead_trace()` method (cargo_test.rs:630-670)
- Creates `TraceCapture` instance
- Writes stdout and stderr to trace files
- Handles errors gracefully with logging

## Test Coverage

All tests pass:
- `trace::tests` - 19/19 passed
- `cargo_test::tests::run_with_bead_trace_*` - 4/4 passed

## Notes

No code changes were needed - the functionality requested in this bead already existed and was fully tested.
