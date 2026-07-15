# Test Run Results - bf-1zxzs

## Summary
- **Exit Code:** 101 (compilation failed)
- **Duration:** 27 seconds
- **Result:** Tests did not run - compilation errors prevented execution

## Compilation Issues

### Errors (4 total)
All errors are non-exhaustive pattern matches in `src/strand/explore.rs`:
- Line 770: `types::StrandResult::NoHomeStore` not covered
- Line 869: `types::StrandResult::NoHomeStore` not covered
- Line 938: `types::StrandResult::NoHomeStore` not covered
- Line 1023: `types::StrandResult::NoHomeStore` not covered

The `NoHomeStore` variant was recently added to `StrandResult` enum in `src/types/mod.rs` but the match statements in `explore.rs` do not handle this case.

### Warnings (22 warnings)
- Unused variables: `last_error`, `tmux_procs`, `tmux_pids`, `registered_procs`, `bead_id`, `global_routing`, `dispatcher`
- Unused imports: Multiple unused imports across test files
- Unused function: `parse_error_line`, `extract_crate_name`
- Unused constant: `WEAVE_AGENT_TIMEOUT_SECS`
- Unreachable patterns: Several duplicate error code patterns in `src/cargo_test.rs`
- Unused doc comments: Multiple doc comments in `tests/routing_matcher_baseline.rs`

## Deliverables
- `test-output-raw.txt` - Full cargo test output (stdout + stderr)
- `test-output-compilation-issues.txt` - Extracted errors and warnings
- `test-exit-code.txt` - Exit code and duration

## Notes
No tests were executed due to the compilation errors. The codebase needs to be fixed by adding the missing `NoHomeStore` pattern arm to the affected match statements in `src/strand/explore.rs` before tests can run.
