# Test Stack Trace Capture

## Overview

NEEDLE provides automated stack trace capture for panicked/failed tests through the `capture_stack_traces.sh` script. This ensures complete debugging information is preserved and organized for developer reference.

## Script: `scripts/capture_stack_traces.sh`

Captures full stack traces for panicked/failed tests with `RUST_BACKTRACE=1` and organizes them by test name.

### Usage

```bash
# Run all tests and capture stack traces to default file (test_stack_traces.txt)
./scripts/capture_stack_traces.sh

# Specify custom output file
./scripts/capture_stack_traces.sh custom_output.txt

# Via environment variable
OUTPUT=path/to/output.txt ./scripts/capture_stack_traces.sh
```

### Features

- **Complete Stack Traces**: Sets `RUST_BACKTRACE=1` for full backtraces
- **Single-Threaded Execution**: Uses `--test-threads=1` to prevent output interleaving
- **Organized by Test Name**: Each failed test gets its own section with:
  - Test name and numbering
  - Thread identifier
  - Source location (file:line:col)
  - Panic/failure message
  - Complete stack backtrace
- **Timeout Protection**: 300-second timeout prevents indefinite hangs
- **Statistics**: Shows total tests run and failed count

### Output Format

```
# NEEDLE Test Stack Traces
Generated: 2026-08-21T08:24:27Z
Environment: RUST_BACKTRACE=1, single-threaded execution

## Test Summary
test result: FAILED. 65 passed; 5 failed; 0 skipped; 0 measured; 0 filtered out

## Statistics
Total tests run: 70
Failed tests: 5

---

## Failed Test Stack Traces

### 1. adapter_validation_rejects_special_characters

**Thread:** adapter_validation_rejects_special_characters (381877)
**Location:** tests/integration_tests.rs:2004:9
**Panic Message:** error message should not execute injected payloads for adapter: '../../../etc/passwd'

**Stack Backtrace:**
```
   0: __rustc::rust_begin_unwind
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
   1: core::panicking::panic_fmt
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
   2: integration_tests::adapter_validation_rejects_special_characters::{{closure}}
             at ./tests/integration_tests.rs:2004:9
   ...
```

---

## Environment Details
- Rust version: rustc 1.75.0
- Cargo version: cargo 1.75.0
- Working directory: /home/coding/NEEDLE
```

## When to Use

- **After test failures**: Automatically run as part of CI/CD to capture failure details
- **Before committing**: Run locally to verify no new test failures have been introduced
- **Debugging flaky tests**: Capture intermittent failure details for analysis
- **Documentation**: Archive stack traces for known issues or edge cases

## Comparison with Other Scripts

| Script | Purpose | Output | Notes |
|--------|---------|--------|-------|
| `capture_stack_traces.sh` | **Organized stack traces** | `test_stack_traces.txt` | **Use this** for debugging test failures |
| `capture_test_output.sh` | Raw test output | `test_output.txt` | Complete stdout/stderr capture |
| `run-tests-with-capture.sh` | Timestamped traces | `.beads/traces/*.log` | For bead-forge CI pipelines |

## Acceptance Criteria Met

✅ Stack traces are preserved in full
✅ Stack traces are organized by test name
✅ Output is saved to a file (`test_stack_traces.txt`)
✅ Stack traces are complete and untruncated

## Implementation Notes

- The script uses `RUST_BACKTRACE=1` (not `full`) for readability - numbered frames are easier to scan than the verbose full output
- Single-threaded execution (`--test-threads=1`) prevents interleaved output that makes parsing difficult
- The 300-second timeout prevents indefinite hangs while allowing time for compilation and test execution
- Output format is markdown-compatible for easy viewing in editors and GitHub
