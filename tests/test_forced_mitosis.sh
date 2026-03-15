#!/usr/bin/env bash
# Test suite for forced mitosis on repeated failure
#
# Tests:
#   - Per-bead failure count tracking
#   - Forced mitosis threshold check
#   - Forced mitosis handling (success path)
#   - Forced mitosis handling (failure path)
#   - Bead failure count reset on success

# Don't use set -e because arithmetic ((++)) can return 1 and trigger exit

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Source required libraries
source "$PROJECT_ROOT/src/lib/constants.sh"
source "$PROJECT_ROOT/src/lib/output.sh"
source "$PROJECT_ROOT/src/lib/json.sh"
source "$PROJECT_ROOT/src/lib/utils.sh"
source "$PROJECT_ROOT/src/telemetry/writer.sh"

# Set up test environment
NEEDLE_HOME="$HOME/.needle-test-forced-mitosis-$$"
NEEDLE_SESSION="test-session-forced-mitosis"
NEEDLE_WORKSPACE="/tmp/test-workspace-forced-mitosis"
NEEDLE_RUNNER="test-runner"
NEEDLE_PROVIDER="test-provider"
NEEDLE_MODEL="test-model"
NEEDLE_IDENTIFIER="test-identifier"
export NEEDLE_VERBOSE=false
export NEEDLE_DEFAULT_RETRY_COUNT=3
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="false"

# Source events module first (loop.sh depends on it)
source "$PROJECT_ROOT/src/telemetry/events.sh"

# Source the loop module (includes the new forced mitosis functions)
source "$PROJECT_ROOT/src/runner/loop.sh"

# Test counters
TESTS_PASSED=0
TESTS_FAILED=0

# ============================================================================
# Test Helpers
# ============================================================================

_test_start() {
    printf 'TEST: %s\n' "$1"
}

_test_pass() {
    printf '  ✓ PASS: %s\n' "$1"
    TESTS_PASSED=$((TESTS_PASSED + 1))
}

_test_fail() {
    printf '  ✗ FAIL: %s\n' "$1"
    [[ -n "${2:-}" ]] && printf '    Details: %s\n' "$2"
    TESTS_FAILED=$((TESTS_FAILED + 1))
}

cleanup() {
    rm -rf "$NEEDLE_HOME"
}
trap cleanup EXIT

# ============================================================================
# Test Cases
# ============================================================================

echo "=========================================="
echo "Running forced mitosis on repeated failure tests"
echo "=========================================="

# ----------------------------------------------------------------------------
# Test 1: Module loads and exports forced mitosis functions
# ----------------------------------------------------------------------------
_test_start "Module loads and exports forced mitosis functions"
functions_ok=true
for fn in \
    _needle_bead_failure_state_file \
    _needle_get_bead_failure_count \
    _needle_increment_bead_failure_count \
    _needle_reset_bead_failure_count \
    _needle_check_forced_mitosis \
    _needle_handle_forced_mitosis; do
    if ! declare -f "$fn" &>/dev/null; then
        _test_fail "Function $fn not defined"
        functions_ok=false
    fi
done
$functions_ok && _test_pass "All forced mitosis functions are defined"

# ----------------------------------------------------------------------------
# Test 2: State file path is correct
# ----------------------------------------------------------------------------
_test_start "State file path is correctly constructed"
state_file=$(_needle_bead_failure_state_file)
expected_path="$NEEDLE_HOME/state/bead_failures.json"
if [[ "$state_file" == "$expected_path" ]]; then
    _test_pass "State file path is correct: $state_file"
else
    _test_fail "State file path incorrect" "expected: $expected_path, got: $state_file"
fi

# ----------------------------------------------------------------------------
# Test 3: Get failure count for non-existent bead returns 0
# ----------------------------------------------------------------------------
_test_start "Get failure count for unknown bead returns 0"
count=$(_needle_get_bead_failure_count "nd-unknown123")
if [[ "$count" -eq 0 ]]; then
    _test_pass "Unknown bead has failure count of 0"
else
    _test_fail "Unknown bead should have count 0, got: $count"
fi

# ----------------------------------------------------------------------------
# Test 4: Increment bead failure count works
# ----------------------------------------------------------------------------
_test_start "Increment bead failure count increments correctly"
test_bead="nd-test-$$-1"
count1=$(_needle_increment_bead_failure_count "$test_bead")
count2=$(_needle_increment_bead_failure_count "$test_bead")
if [[ "$count1" -eq 1 ]] && [[ "$count2" -eq 2 ]]; then
    _test_pass "Failure count increments: $count1 -> $count2"
else
    _test_fail "Failure count increment failed" "got: $count1, $count2"
fi

# ----------------------------------------------------------------------------
# Test 5: Get failure count returns tracked value
# ----------------------------------------------------------------------------
_test_start "Get failure count returns tracked value"
test_bead="nd-test-$$-2"
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_needle_increment_bead_failure_count "$test_bead" >/dev/null
count=$(_needle_get_bead_failure_count "$test_bead")
if [[ "$count" -eq 3 ]]; then
    _test_pass "Failure count correctly tracked: 3"
else
    _test_fail "Failure count should be 3, got: $count"
fi

# ----------------------------------------------------------------------------
# Test 6: Reset bead failure count clears to 0
# ----------------------------------------------------------------------------
_test_start "Reset bead failure count clears to 0"
test_bead="nd-test-$$-3"
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_needle_reset_bead_failure_count "$test_bead"
count=$(_needle_get_bead_failure_count "$test_bead")
if [[ "$count" -eq 0 ]]; then
    _test_pass "Failure count reset to 0"
else
    _test_fail "Failure count should be 0 after reset, got: $count"
fi

# ----------------------------------------------------------------------------
# Test 7: Multiple beads have independent failure counts
# ----------------------------------------------------------------------------
_test_start "Multiple beads track independent failure counts"
bead1="nd-test-$$-a"
bead2="nd-test-$$-b"
_needle_increment_bead_failure_count "$bead1" >/dev/null
_needle_increment_bead_failure_count "$bead1" >/dev/null
_needle_increment_bead_failure_count "$bead2" >/dev/null
count1=$(_needle_get_bead_failure_count "$bead1")
count2=$(_needle_get_bead_failure_count "$bead2")
if [[ "$count1" -eq 2 ]] && [[ "$count2" -eq 1 ]]; then
    _test_pass "Beads have independent counts: $count1 and $count2"
else
    _test_fail "Independent counts failed" "bead1=$count1 (expected 2), bead2=$count2 (expected 1)"
fi

# ----------------------------------------------------------------------------
# Test 8: State file persists across multiple operations
# ----------------------------------------------------------------------------
_test_start "State file persists correctly with multiple operations"
test_bead="nd-test-$$-p1"
# Create multiple test beads to test persistence
for i in {1..5}; do
    _needle_increment_bead_failure_count "$test_bead" >/dev/null
done
final_count=$(_needle_get_bead_failure_count "$test_bead")
if [[ "$final_count" -eq 5 ]]; then
    _test_pass "State file persists correctly: count is 5"
else
    _test_fail "State file persistence failed" "expected 5, got: $final_count"
fi

# ----------------------------------------------------------------------------
# Test 9: Bead failure count is independent across test runs (simulated)
# ----------------------------------------------------------------------------
_test_start "Multiple operations on same bead accumulate correctly"
test_bead="nd-test-$$-acc"
count1=$(_needle_increment_bead_failure_count "$test_bead")
count2=$(_needle_increment_bead_failure_count "$test_bead")
count3=$(_needle_increment_bead_failure_count "$test_bead")
if [[ "$count1" -eq 1 ]] && [[ "$count2" -eq 2 ]] && [[ "$count3" -eq 3 ]]; then
    _test_pass "Multiple operations accumulate: 1 -> 2 -> 3"
else
    _test_fail "Accumulation failed" "got: $count1, $count2, $count3"
fi

# ----------------------------------------------------------------------------
# Test 10: Reset after successful mitosis
# ----------------------------------------------------------------------------
_test_start "Bead failure count is reset after successful mitosis"
test_bead="nd-test-$$-7"
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_needle_increment_bead_failure_count "$test_bead" >/dev/null
count_before=$(_needle_get_bead_failure_count "$test_bead")
# Simulate mitosis success by directly resetting
_needle_reset_bead_failure_count "$test_bead"
count_after=$(_needle_get_bead_failure_count "$test_bead")
if [[ "$count_before" -eq 2 ]] && [[ "$count_after" -eq 0 ]]; then
    _test_pass "Failure count reset after mitosis: $count_before -> 0"
else
    _test_fail "Reset failed" "before=$count_before, after=$count_after"
fi

# ----------------------------------------------------------------------------
# Test 11: _needle_check_forced_mitosis returns false when force disabled
# ----------------------------------------------------------------------------
_test_start "_needle_check_forced_mitosis returns false when force_on_failure disabled"
test_bead="nd-test-$$-fm1"
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_NEEDLE_MITOSIS_LOADED=1
_needle_mitosis_force_enabled() { return 1; }
_needle_mitosis_force_threshold() { echo "3"; }
if ! _needle_check_forced_mitosis "$test_bead" "$NEEDLE_WORKSPACE" 2>/dev/null; then
    _test_pass "Returns false when force_on_failure is disabled (even with count >= threshold-1)"
else
    _test_fail "Expected false when force disabled, got true"
fi
unset -f _needle_mitosis_force_enabled _needle_mitosis_force_threshold
unset _NEEDLE_MITOSIS_LOADED

# ----------------------------------------------------------------------------
# Test 12: _needle_check_forced_mitosis returns false when below threshold
# ----------------------------------------------------------------------------
_test_start "_needle_check_forced_mitosis returns false when below threshold"
test_bead="nd-test-$$-fm2"
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_NEEDLE_MITOSIS_LOADED=1
_needle_mitosis_force_enabled() { return 0; }
_needle_mitosis_force_threshold() { echo "3"; }
if ! _needle_check_forced_mitosis "$test_bead" "$NEEDLE_WORKSPACE" 2>/dev/null; then
    _test_pass "Returns false when below threshold (1 failure, threshold 3)"
else
    _test_fail "Expected false below threshold, got true"
fi
unset -f _needle_mitosis_force_enabled _needle_mitosis_force_threshold
unset _NEEDLE_MITOSIS_LOADED

# ----------------------------------------------------------------------------
# Test 13: _needle_check_forced_mitosis returns true at threshold-1
# ----------------------------------------------------------------------------
_test_start "_needle_check_forced_mitosis returns true at threshold-1 failures"
test_bead="nd-test-$$-fm3"
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_NEEDLE_MITOSIS_LOADED=1
_needle_mitosis_force_enabled() { return 0; }
_needle_mitosis_force_threshold() { echo "3"; }
if _needle_check_forced_mitosis "$test_bead" "$NEEDLE_WORKSPACE" 2>/dev/null; then
    _test_pass "Returns true when failure count reaches threshold-1 (2 failures, threshold 3)"
else
    _test_fail "Expected true at threshold-1, got false"
fi
unset -f _needle_mitosis_force_enabled _needle_mitosis_force_threshold
unset _NEEDLE_MITOSIS_LOADED

# ----------------------------------------------------------------------------
# Test 14: _needle_handle_forced_mitosis calls _needle_check_mitosis with force=true
# ----------------------------------------------------------------------------
_test_start "_needle_handle_forced_mitosis calls _needle_check_mitosis with force=true"
test_bead="nd-test-$$-fm4"
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_NEEDLE_MITOSIS_LOADED=1
_fm4_force_arg=""
_needle_check_mitosis() {
    _fm4_force_arg="${4:-false}"
    return 0
}
_needle_handle_forced_mitosis "$test_bead" "$NEEDLE_WORKSPACE" "test-agent" >/dev/null 2>&1
if [[ "$_fm4_force_arg" == "true" ]]; then
    _test_pass "_needle_check_mitosis called with force=true"
else
    _test_fail "Expected force=true, got: $_fm4_force_arg"
fi
unset -f _needle_check_mitosis
unset _NEEDLE_MITOSIS_LOADED _fm4_force_arg

# ----------------------------------------------------------------------------
# Test 15: _needle_handle_forced_mitosis resets failure count on success
# ----------------------------------------------------------------------------
_test_start "_needle_handle_forced_mitosis resets failure count on mitosis success"
test_bead="nd-test-$$-fm5"
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_NEEDLE_MITOSIS_LOADED=1
_needle_check_mitosis() { return 0; }
_needle_handle_forced_mitosis "$test_bead" "$NEEDLE_WORKSPACE" "test-agent" >/dev/null 2>&1
count_after=$(_needle_get_bead_failure_count "$test_bead")
if [[ "$count_after" -eq 0 ]]; then
    _test_pass "Failure count reset to 0 after successful mitosis"
else
    _test_fail "Expected 0 after successful mitosis, got: $count_after"
fi
unset -f _needle_check_mitosis
unset _NEEDLE_MITOSIS_LOADED

# ----------------------------------------------------------------------------
# Test 16: _needle_handle_forced_mitosis resets failure count when mitosis fails
# ----------------------------------------------------------------------------
_test_start "_needle_handle_forced_mitosis resets failure count when mitosis cannot decompose"
test_bead="nd-test-$$-fm6"
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_needle_increment_bead_failure_count "$test_bead" >/dev/null
_NEEDLE_MITOSIS_LOADED=1
_needle_check_mitosis() { return 1; }
_needle_handle_forced_mitosis "$test_bead" "$NEEDLE_WORKSPACE" "test-agent" >/dev/null 2>&1
count_after=$(_needle_get_bead_failure_count "$test_bead")
if [[ "$count_after" -eq 0 ]]; then
    _test_pass "Failure count reset to 0 when mitosis cannot decompose (atomic bead)"
else
    _test_fail "Expected 0 when mitosis fails, got: $count_after"
fi
unset -f _needle_check_mitosis
unset _NEEDLE_MITOSIS_LOADED

# ============================================================================
# Summary
# ============================================================================

echo ""
echo "=========================================="
echo "Test Results"
echo "=========================================="
echo "Passed: $TESTS_PASSED"
echo "Failed: $TESTS_FAILED"
echo "=========================================="

if [[ $TESTS_FAILED -eq 0 ]]; then
    echo "✓ All tests passed!"
    exit 0
else
    echo "✗ Some tests failed"
    exit 1
fi
