# Failing Tests Report

## Summary Statistics

- **Total tests run**: 1418
- **Tests passed**: 1416
- **Tests failed**: 2
- **Tests ignored**: 0
- **Duration**: 605.21s (first run), 625.53s (second run)

## Test Results by Type

### Failure Type: Panic (Assertion Failure)
Both failing tests are assertion panics, not unexpected errors or timeouts.

## Detailed Failure List

### 1. `bead_store::tests::detects_locked_db_error`

**Module**: `bead_store`
**File**: `src/bead_store/mod.rs:1477:9`
**Thread ID**: 575932 (first run), 591734 (second run)

**Error Type**: Panic - Assertion failure
**Full Error Message**:
```
thread 'bead_store::tests::detects_locked_db_error' (575932) panicked at src/bead_store/mod.rs:1477:9:
assertion failed: is_corruption_error("database is locked")
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

**What failed**: The test asserts that `is_corruption_error("database is locked")` should return `true`, but it returned `false`.

**Issue**: The `is_corruption_error` function does not recognize "database is locked" as a corruption error. This suggests the function's error pattern matching may be incomplete or the test expectation is wrong.

---

### 2. `cli::tests::find_all_descendants_handles_cycles`

**Module**: `cli`
**File**: `src/cli/mod.rs:5755:9`
**Thread ID**: 576708 (first run), 592536 (second run)

**Error Type**: Panic - Assertion failure (left != right)
**Full Error Message**:
```
thread 'cli::tests::find_all_descendants_handles_cycles' (576708) panicked at src/cli/mod.rs:5755:9:
assertion `left == right` failed
  left: 2
 right: 1
```

**What failed**: The test expected a value of `1` but got `2`.

**Issue**: The `find_all_descendants_handles_cycles` test is supposed to handle cycles in the process tree, but it's returning 2 descendants instead of the expected 1. This suggests the cycle detection logic may not be working correctly - it might be counting a node twice or not properly deduplicating when a cycle is detected.

---

## Additional Compiler Warnings (Non-failing)

The test run also produced some compiler warnings that don't cause test failures but may indicate code quality issues:

### Unused Variable Warning
**File**: `src/bead_store/mod.rs:1001:30`
```
warning: value assigned to `last_error` is never read
    --> src/bead_store/mod.rs:1001:30
     |
1001 |         let mut last_error = None;
     |                              ^^^^ this value is reassigned later and never used
```

**Also at**: `src/bead_store/mod.rs:1089:13`

The variable `last_error` is assigned but never read before being reassigned.

### Unreachable Pattern Warning
**File**: `src/cargo_test.rs:229:33`
```
warning: unreachable pattern
   --> src/cargo_test.rs:229:33
    |
213 |             | "E0521" | "E0623" | "E0383" | "E0503" | "E0504" | "E0510" | "E0391" | "E0392"
    |                         ------- matches all the relevant values
```

An earlier pattern in a match arm makes a later pattern unreachable.

## Conclusion

- **Total failure count**: 2 tests
- **Failure classification**: Both are assertion panics (test failures, not runtime errors)
- **No timeouts**: All tests completed within the time limit
- **No unexpected crashes**: All failures are deliberate assertion checks that failed

The two failures are:
1. **bead_store::tests::detects_locked_db_error** - Error detection function doesn't recognize "database is locked" as corruption
2. **cli::tests::find_all_descendants_handles_cycles** - Cycle detection returns wrong count (2 instead of 1)
