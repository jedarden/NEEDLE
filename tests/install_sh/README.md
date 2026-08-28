# install.sh Checksum Verification Tests

Regression tests for the checksum verification logic in `install.sh`.

## Overview

These tests verify that the checksum verification in `install.sh` works correctly across various scenarios:

- ✅ Valid checksum verification
- ❌ Missing checksums file
- ❌ Checksum for asset not found
- ❌ Checksum mismatch (never skippable)
- ❌ Missing SHA-256 tools (sha256sum, shasum)
- ⚠️  Opt-out flag behavior (`--skip-checksum`, `NEEDLE_SKIP_CHECKSUM`)
- ℹ️  GPG signature verification (informational only)

## Prerequisites

### Required: Bats (Bash Automated Testing System)

Install Bats to run these tests:

```bash
# On Ubuntu/Debian
sudo apt install bats

# On macOS
brew install bats-core

# Or from source
git clone https://github.com/bats-core/bats-core.git
cd bats-core
sudo ./install.sh /usr/local
```

### Optional: Checksum Tools

The tests use real checksum tools when available:
- `sha256sum` (from coreutils)
- `shasum` (from perl)

Install one or both for full test coverage:

```bash
# Ubuntu/Debian
sudo apt install coreutils perl

# macOS
brew install coreutils
```

## Running Tests

### Quick Start (No Bats Required)

The standalone runner works without any dependencies:

```bash
cd /home/coding/NEEDLE/tests/install_sh
./run_tests_standalone.sh
```

With verbose output:

```bash
./run_tests_standalone.sh -v
```

List all available tests:

```bash
./run_tests_standalone.sh -l
```

Run tests matching a pattern:

```bash
./run_tests_standalone.sh -f "checksum"
```

### With Bats (Optional)

If you have Bats installed, you can use the Bats test file:

### Run with verbose output:

```bash
bats -t checksum_verification.bats
```

### Run specific tests:

```bash
./run_tests_standalone.sh -f "valid checksum"
./run_tests_standalone.sh -f "missing checksums"
```

### With Bats (Optional)

If you have Bats installed:

## Test Structure

```
tests/install_sh/
├── README.md                          # This file
├── checksum_verification.bats         # Bats test suite (optional, requires Bats)
├── test_helpers.bash                  # Helper functions for Bats tests
├── run_tests_standalone.sh            # Standalone runner (no dependencies)
└── fixtures/                          # Created dynamically during tests
```

### Test Runners

**Standalone Runner (Recommended)**
- `run_tests_standalone.sh` - Pure shell, no dependencies required
- Works on any system with bash
- Use this for CI and quick checks

**Bats Runner (Optional)**
- `checksum_verification.bats` - Requires Bats
- More feature-rich with better reporting
- Use if you already have Bats installed

## Test Scenarios

### 1. Valid Checksum Verification
Tests that a correct checksum passes verification:
```bash
bats -f "valid checksum verification" checksum_verification.bats
```

### 2. Missing Checksums File
Tests behavior when checksums.txt cannot be downloaded:
- **Without `--skip-checksum`**: Should fail
- **With `--skip-checksum`**: Should succeed with warning

```bash
bats -f "missing checksums file" checksum_verification.bats
```

### 3. Checksum for Asset Not Found
Tests when the expected asset isn't in checksums.txt:
- **Without `--skip-checksum`**: Should fail
- **With `--skip-checksum`**: Should succeed with warning

```bash
bats -f "checksum for asset not found" checksum_verification.bats
```

### 4. Checksum Mismatch
Tests that a mismatched checksum **always fails**, even with `--skip-checksum`:
- This is a security-critical scenario
- Mismatches are never skippable by design

```bash
bats -f "checksum mismatch" checksum_verification.bats
```

### 5. Missing SHA-256 Tools
Tests behavior when neither `sha256sum` nor `shasum` is available:
- **Without `--skip-checksum`**: Should fail
- **With `--skip-checksum`**: Should succeed with warning

```bash
bats -f "missing SHA-256 tool" checksum_verification.bats
```

### 6. Opt-Out Flag Behavior
Tests that the opt-out mechanisms work correctly:
- Command-line flag: `--skip-checksum`
- Environment variable: `NEEDLE_SKIP_CHECKSUM=1`
- Value normalization (1, true, yes → all treated as "true")

```bash
bats -f "opt-out" checksum_verification.bats
```

### 7. GPG Verification
Tests that GPG signature verification is informational only (never fails installation):

```bash
bats -f "GPG" checksum_verification.bats
```

## Isolation and Safety

These tests are **fully isolated** and **safe to run**:

- ✅ Use temporary directories (`mktemp -d`) for each test
- ✅ Mock binaries and checksums (no real downloads)
- ✅ No modifications to real user installation
- ✅ No network access required
- ✅ Automatic cleanup via `teardown()`

## CI Integration

Add to CI workflow (e.g., `.github/workflows/test.yml` or Argo WorkflowTemplate):

```yaml
steps:
  - name: Run install.sh checksum tests
    run: |
      cd /home/coding/NEEDLE/tests/install_sh
      ./run_tests_standalone.sh
```

No additional dependencies needed! The standalone runner uses only bash.

## Troubleshooting

### Tests fail with "Bats not found"
```bash
# Install Bats
sudo apt install bats
```

### Tests skip due to missing checksum tools
```bash
# Install checksum tools for full coverage
sudo apt install coreutils perl
```

### Test output is hard to read
```bash
# Use verbose tap output
bats -t checksum_verification.bats
```

### Tests leave temporary files
The tests use a trap to clean up, but if interrupted:
```bash
# Find and remove temp directories
find /tmp -name "tmp.*" -type d -mtime +1 -exec rm -rf {} \;
```

## Adding New Tests

To add a new test scenario:

1. Add a new `@test` block to `checksum_verification.bats`
2. Use helper functions from `test_helpers.bash`
3. Ensure cleanup in `teardown()`
4. Document in this README

Example:
```bash
@test "new test scenario" {
    # Arrange
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "content" > "$test_binary"

    # Act
    run verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums.txt" "asset-name"

    # Assert
    [ "$status" -eq 0 ]
    [[ "$output" == *"expected output"* ]]
}
```

## References

- Main installer: `/home/coding/NEEDLE/install.sh`
- Bats documentation: https://bats-core.readthedocs.io/
- ADR on checksum security: NEEDLE/docs/adr/XXX-checksum-security.md (if exists)

## License

Same as parent NEEDLE project.
