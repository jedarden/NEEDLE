# NEEDLE Installer Tests

Isolated, shell-level regression tests for the NEEDLE installer script (`install.sh`).

## Overview

These tests verify the behavior of the installer script across all critical paths, with a focus on security-critical checksum verification and opt-out mechanisms. All tests are fully isolated and do not touch real user installations or make network calls.

The authoritative suite is `test_e2e_install.sh`: it runs the **real** `install.sh`
as a subprocess against a mock `curl` that serves the GitHub API document and
release assets from a local fixture root (missing files behave like `curl -f`
on a 404). Every run gets an empty environment (`env -i`), its own `HOME`, its
own `NEEDLE_INSTALL_PATH`, and closed stdin — the exact shape of the documented
`curl ... | bash` invocation. The other two suites are supplementary
(fixture-level and structural checks).

## Running Tests

### Run installer tests only
```bash
make test-install
# OR
bash tests/installer/run.sh
# OR
bash tests/installer/test_install.sh
```

### Run all tests (includes installer tests)
```bash
make test-all
# OR
bash scripts/definition-of-done.sh --all
```

## Test Coverage

Behaviors below are exercised end-to-end by `test_e2e_install.sh` (real script,
mock download layer); the fixture suites provide supplementary checks.

### Checksum Verification (Security-Critical)
- ✅ **Valid checksums** - Installation proceeds with correct checksum
- ✅ **Mismatched checksums** - Installation aborts on mismatch (never skippable)
- ✅ **Missing checksum entries** - Installation aborts when asset not found in checksums file
- ✅ **Checksum download failures** - Properly handled with/without opt-out

### Opt-Out Mechanisms
- ✅ **`--skip-checksum` flag** - Recognized and parsed correctly
- ✅ **`NEEDLE_SKIP_CHECKSUM` env var** - Multiple values normalized (1, true, yes, etc.)
- ✅ **Opt-out security warning** - Conspicuous warning displayed when checksum verification disabled

### Tool Availability
- ✅ **Missing SHA-256 tools** - Graceful failure when `sha256sum`/`shasum` unavailable
- ✅ **Tool detection** - Correctly identifies available hash tools

### Installation Safety
- ✅ **Temp directory usage** - Uses `mktemp -d` for isolated temp space
- ✅ **Cleanup trap** - Sets EXIT trap to clean up temp directory
- ✅ **Binary verification** - Verifies downloaded binary works before installation
- ✅ **PATH configuration** - Detects and reports when install dir not in PATH

### Platform Detection
- ✅ **OS detection** - Correctly identifies Linux vs macOS
- ✅ **Architecture detection** - Correctly identifies x86_64 vs aarch64

### Download Failures
- ✅ **Checksum download failure** - Aborts without opt-out, continues with opt-out
- ✅ **Binary download failure** - Aborts before anything is moved into place
- ✅ **GitHub API unreachable** - Aborts with a legible message
- ✅ **Version discovery** - Parses a >64KiB multi-line release document with no
  `curl: (23) Failure writing output` / broken-pipe diagnostic

### Opt-Out Robustness
- ✅ **Non-interactive opt-out** - `--skip-checksum` / `NEEDLE_SKIP_CHECKSUM=1`
  with closed stdin installs (regression: the confirmation prompt's `read`
  used to hit EOF and abort under `set -e`)
- ✅ **Missing checksum entry + opt-out** - Installs with warning (regression:
  the entry-lookup grep used to abort silently under `pipefail` before the
  opt-out branch could apply)

## Parallel Safety

All tests are designed to run in parallel without conflicts:

1. **Unique temp directories** - Each test uses a unique temp directory (`mktemp -t TEST-ID-XXXXXX`)
2. **Isolated environment** - Tests export their own `HOME` and `NEEDLE_INSTALL_PATH`
3. **No shared state** - Tests don't write to any shared locations
4. **Process-safe naming** - Test IDs include PID and random number for uniqueness

## Test Architecture

### Test Framework
Custom bash test framework with helpers:
- `setup()` - Creates isolated temp directory
- `teardown()` - Cleans up temp directory
- `assert_eq()` - String equality assertions
- `assert_contains()` - Substring assertions
- `assert_exit_code()` - Exit code assertions

### Fixtures and Mocks
- `create_mock_binary()` - Creates mock binary with known content
- `create_mock_checksums()` - Creates mock checksums file with known hashes
- Mock PATH manipulation - Removes hash tools to test error handling
- Mock install scripts - Simulates various failure modes

### Test Files
- `test_e2e_install.sh` - End-to-end suite: runs the real install.sh against a mock curl (14 scenarios)
- `test_install.sh` - Fixture-level test suite (25 tests)
- `test_checksum_verification.sh` - Legacy fixture suite, retained for reference
- `run.sh` - Test runner script (runs all three suites, e2e first)
- `README.md` - This documentation

## Security-Critical Behaviors

The following behaviors are **security-critical** and never subject to opt-out:

1. **Checksum mismatches** - Always abort installation, never skippable
2. **Binary verification** - Always verify `--version` before installation
3. **Tamper detection** - Any sign of tampering aborts installation

The `--skip-checksum` flag and `NEEDLE_SKIP_CHECKSUM` env var **only** apply to:
- Missing checksums.txt file (download failure)
- Missing checksum entry for the asset
- Missing hash tools (sha256sum/shasum)

## Integration

The installer tests are integrated into the standard test runner:

- **Slow lane CI** - Runs in `scripts/definition-of-done.sh --slow`
- **Make targets** - Available via `make test-install` and `make test-all`
- **GitHub Actions** - Included in CI pipeline (when enabled)

## Adding New Tests

To add a new test:

1. Create a test function: `test_your_scenario() { ... }`
2. Call `setup()` at the start
3. Use `assert_*()` helpers for assertions
4. Call `teardown()` at the end
5. Add the test to `main()` function

Example:
```bash
test_new_feature() {
    echo "TEST: test_new_feature"
    
    setup
    local mock_dir="$tmp_dir/mock"
    mkdir -p "$mock_dir"
    
    # Your test logic here
    assert_eq "expected" "actual" "feature works correctly"
    
    teardown
}
```

## Debugging Failed Tests

To debug a specific failing test:

1. Run the test suite to see which test failed
2. Comment out the `teardown()` call in that test
3. Run again - temp directory won't be cleaned up
4. Inspect `$tmp_dir` contents manually
5. Remember to uncomment `teardown()` when done

## Legacy Tests

The original `test_checksum_verification.sh` is retained for reference but superseded by `test_install.sh`. The new suite provides:
- More comprehensive coverage (25 vs 16 tests)
- Better parallel safety
- Clearer test organization
- Better documentation
