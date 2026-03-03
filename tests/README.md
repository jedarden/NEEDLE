# NEEDLE Test Suite

This directory contains comprehensive tests for the NEEDLE worker system.

## Running Tests

### Run All Tests

```bash
# Run all test files
for test in tests/test_*.sh; do bash "$test" || exit 1; done
```

### Run Individual Tests

```bash
# Run a specific test file
bash tests/test_runner.sh
bash tests/test_runner_loop.sh
bash tests/test_heartbeat.sh
bash tests/test_state.sh
```

## Test Files

### Core Worker Tests

| File | Description | Tests |
|------|-------------|-------|
| `test_runner.sh` | Comprehensive worker loop tests | 59 tests covering basic loop, error handling, concurrency, configuration, backoff & crash recovery |
| `test_runner_loop.sh` | Worker loop module tests | Configuration, events, telemetry, heartbeat, bead functions |
| `test_state.sh` | Worker state registry tests | Registration, counting, cleanup |
| `test_heartbeat.sh` | Heartbeat monitoring tests | Init, keepalive, status updates |
| `test_loop_cleanup.sh` | Loop cleanup tests | Cleanup after processing |

### Component Tests

| File | Description |
|------|-------------|
| `test_config.sh` | Configuration loading tests |
| `test_events.sh` | Event emission tests |
| `test_tokens.sh` | Token counting tests |
| `test_budget.sh` | Budget tracking tests |
| `test_effort.sh` | Effort estimation tests |
| `test_dispatch.sh` | Agent dispatch tests |
| `test_claim.sh` | Bead claim tests |
| `test_select.sh` | Bead selection tests |
| `test_priority.sh` | Priority handling tests |

### Utility Tests

| File | Description |
|------|-------------|
| `test_escape.sh` | String escaping tests |
| `test_json.sh` | JSON handling tests |
| `test_utils.sh` | Utility function tests |
| `test_output.sh` | Output formatting tests |

## Test Categories

### Basic Loop Tests (test_runner.sh)

- Worker initialization
- Strand engine integration
- Heartbeat during loop
- Graceful shutdown signal handling
- Shutdown triggers draining state

### Error Handling Tests (test_runner.sh)

- Agent failure recovery
- Exit code handling (0, 1, 124, unknown)
- Missing bead handling
- Bead claim failure handling
- Retry on transient error
- Release on fatal error

### Concurrency Tests (test_runner.sh)

- Max concurrent configuration
- Worker registration
- Worker count function
- No race condition on bead claim
- Concurrent worker coordination

### Configuration Tests (test_runner.sh)

- Config hot-reload detection
- Config hot-reload trigger on change
- Config mtime function
- Config validation (valid/empty)
- Workspace config override
- Config fallback to default

### Backoff & Crash Recovery Tests (test_runner.sh)

- Backoff reset
- Backoff increment
- Backoff exponential growth
- Backoff max cap
- Alert human threshold
- Max failures exit
- Crash loop alert function

## Writing New Tests

### Test File Template

```bash
#!/usr/bin/env bash
# Test script for [module name]
set -o pipefail

# Test directory
TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$TEST_DIR")"

# Test counters
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

# Test helpers
test_start() {
    local name="$1"
    ((TESTS_RUN++))
    echo -n "Testing: $name... "
}

test_pass() {
    echo "PASS"
    ((TESTS_PASSED++))
}

test_fail() {
    local reason="${1:-}"
    echo "FAIL"
    [[ -n "$reason" ]] && echo "  Reason: $reason"
    ((TESTS_FAILED++))
}

# Cleanup
cleanup() {
    # Remove temp files
}
trap cleanup EXIT

# Tests here...

# Summary
echo ""
echo "=========================================="
echo "Test Results"
echo "=========================================="
echo "Tests run:    $TESTS_RUN"
echo "Tests passed: $TESTS_PASSED"
echo "Tests failed: $TESTS_FAILED"

if [[ $TESTS_FAILED -eq 0 ]]; then
    echo "✓ All tests passed!"
    exit 0
else
    echo "✗ Some tests failed"
    exit 1
fi
```

### Best Practices

1. **Isolation**: Each test should run independently
2. **Cleanup**: Always clean up temp files and directories
3. **Mocking**: Use mock functions for external dependencies (br CLI, agents)
4. **Exit codes**: Return 0 on success, non-zero on failure
5. **Output**: Show pass/fail for each test

## Acceptance Criteria

For test suites implementing new features:

- [ ] 10+ test cases covering core logic
- [ ] Tests run in isolation (mock external dependencies)
- [ ] Exit code 0 on all pass, non-zero on failure
- [ ] Output shows pass/fail for each test
- [ ] Can run individual tests
- [ ] Documented in this README.md
