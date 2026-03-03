#!/usr/bin/env bash
# Test script for strands/knot.sh module

# Don't use set -e because arithmetic ((++)) can return 1 and trigger exit

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Source required libraries
source "$PROJECT_ROOT/src/lib/constants.sh"
source "$PROJECT_ROOT/src/lib/output.sh"
source "$PROJECT_ROOT/src/lib/paths.sh"
source "$PROJECT_ROOT/src/lib/json.sh"
source "$PROJECT_ROOT/src/lib/utils.sh"
source "$PROJECT_ROOT/src/lib/config.sh"

# Set up test environment
NEEDLE_HOME="$HOME/.needle-test-knot-$$"
NEEDLE_SESSION="test-knot-$$"
NEEDLE_WORKSPACE="/tmp/test-workspace-knot"
NEEDLE_AGENT="test-agent"
NEEDLE_VERBOSE=true
NEEDLE_STATE_DIR="state"
NEEDLE_LOG_DIR="logs"
NEEDLE_LOG_FILE="$NEEDLE_HOME/$NEEDLE_LOG_DIR/$(date +%Y-%m-%d).jsonl"

# Create test directories
mkdir -p "$NEEDLE_HOME/$NEEDLE_STATE_DIR"
mkdir -p "$NEEDLE_HOME/$NEEDLE_LOG_DIR"
mkdir -p "$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"

# Create a test workspace directory for pre-flight verification tests
# (the knot.sh code does `cd "$workspace"` which requires the directory to exist)
TEST_WORKSPACE_DIR="/tmp/needle-test-workspace-$$"
mkdir -p "$TEST_WORKSPACE_DIR/bin"

# Create a minimal config file for testing
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
strands:
  pluck: true
  explore: true
  mend: true
  weave: false
  unravel: false
  pulse: false
  knot: true

knot:
  rate_limit_interval: 3600
EOF

# Source the knot module
source "$PROJECT_ROOT/src/strands/knot.sh"

# Test counters
TESTS_PASSED=0
TESTS_FAILED=0

# Test helper functions
_test_start() {
    echo "TEST: $1"
}

_test_pass() {
    echo "  ✓ PASS: $1"
    TESTS_PASSED=$((TESTS_PASSED + 1))
}

_test_fail() {
    echo "  ✗ FAIL: $1"
    [[ -n "$2" ]] && echo "    Details: $2"
    TESTS_FAILED=$((TESTS_FAILED + 1))
}

# Mock br command for testing
# Default behavior: no claimable beads (empty arrays)
# Tests can override by setting MOCK_BR_READY_COUNT or MOCK_BR_LIST_DATA
#
# IMPORTANT: This function must be exported to be available in subshells
# (the knot.sh code runs br commands via $(cd ... && br ...) which creates subshells)
br() {
    case "$1" in
        ready)
            # Return claimable beads count for false positive prevention
            if [[ -n "$MOCK_BR_READY_COUNT" ]]; then
                # Return array of mock beads
                local count="$MOCK_BR_READY_COUNT"
                local result="["
                for ((i=0; i<count; i++)); do
                    [[ $i -gt 0 ]] && result+=","
                    result+="{\"id\":\"nd-mock-$i\",\"title\":\"Mock bead $i\",\"priority\":2}"
                done
                result+="]"
                echo "$result"
            else
                echo '[]'
            fi
            ;;
        list)
            if [[ -n "$MOCK_BR_LIST_DATA" ]]; then
                echo "$MOCK_BR_LIST_DATA"
            else
                echo '[]'
            fi
            ;;
        create)
            # Return a mock bead ID
            echo "nd-knot-test-$$"
            return 0
            ;;
        *)
            return 0
            ;;
    esac
}

# Export the mock br function and mock variables so they're available in subshells
export -f br
export MOCK_BR_READY_COUNT MOCK_BR_LIST_DATA

# Cleanup function
cleanup() {
    rm -rf "$NEEDLE_HOME"
    rm -rf "$TEST_WORKSPACE_DIR"
}
trap cleanup EXIT

# Run tests
echo "=========================================="
echo "Running strands/knot.sh tests"
echo "=========================================="

# Test 1: Rate limit allows first call
_test_start "Rate limit allows first call"
if _needle_knot_check_rate_limit "/workspace/test1"; then
    _test_pass "Rate limit allows first call"
else
    _test_fail "Rate limit incorrectly blocked first call"
fi

# Test 2: Rate limit blocks subsequent calls within interval
_test_start "Rate limit blocks subsequent calls"
_needle_knot_record_alert "/workspace/test1"
if ! _needle_knot_check_rate_limit "/workspace/test1"; then
    _test_pass "Rate limit correctly blocked subsequent call"
else
    _test_fail "Rate limit failed to block subsequent call"
fi

# Test 3: Rate limit allows calls after interval
_test_start "Rate limit allows calls after interval"
# Set a timestamp 2 hours ago
workspace_hash=$(echo "/workspace/test2" | md5sum | cut -c1-8)
old_ts=$(($(date +%s) - 7200))
echo "$old_ts" > "$NEEDLE_HOME/$NEEDLE_STATE_DIR/knot_alert_${workspace_hash}"
if _needle_knot_check_rate_limit "/workspace/test2"; then
    _test_pass "Rate limit correctly allowed call after interval"
else
    _test_fail "Rate limit incorrectly blocked call after interval"
fi

# Test 4: Different workspaces have independent rate limits
_test_start "Different workspaces have independent rate limits"
_needle_knot_record_alert "/workspace/test3"
if _needle_knot_check_rate_limit "/workspace/test4"; then
    _test_pass "Different workspace not rate limited"
else
    _test_fail "Different workspace incorrectly rate limited"
fi

# Test 5: Rate limit clear function works
_test_start "Rate limit clear function works"
_needle_knot_record_alert "/workspace/test5"
if ! _needle_knot_check_rate_limit "/workspace/test5"; then
    _needle_knot_clear_rate_limit "/workspace/test5"
    if _needle_knot_check_rate_limit "/workspace/test5"; then
        _test_pass "Rate limit clear function works"
    else
        _test_fail "Rate limit still blocked after clear"
    fi
else
    _test_fail "Rate limit was not set up correctly for clear test"
fi

# Test 6: Diagnostic collection produces output
_test_start "Diagnostic collection produces output"
diag=$(_needle_knot_collect_diagnostics "/test/workspace" "test-agent")
if [[ -n "$diag" ]] && echo "$diag" | grep -q "Strand Configuration"; then
    _test_pass "Diagnostic collection produces expected output"
else
    _test_fail "Diagnostic collection output missing expected content"
fi

# Test 7: Stats function returns valid JSON
_test_start "Stats function returns valid JSON"
stats=$(_needle_knot_stats)
if echo "$stats" | jq -e . >/dev/null 2>&1; then
    _test_pass "Stats function returns valid JSON"
else
    _test_fail "Stats function returned invalid JSON: $stats"
fi

# Test 8: Stats function includes expected fields
_test_start "Stats function includes expected fields"
stats=$(_needle_knot_stats)
if echo "$stats" | jq -e 'has("alert_tracking_files") and has("last_alert")' >/dev/null 2>&1; then
    _test_pass "Stats function includes expected fields"
else
    _test_fail "Stats function missing expected fields"
fi

# Test 9: Main strand function returns failure when rate limited
_test_start "Main strand function returns failure when rate limited"
_needle_knot_record_alert "/workspace/rate-limited"
if ! _needle_strand_knot "/workspace/rate-limited" "test-agent"; then
    _test_pass "Strand correctly returned failure when rate limited"
else
    _test_fail "Strand should have returned failure when rate limited"
fi

# Test 10: Workspace hash is consistent
_test_start "Workspace hash is consistent"
hash1=$(echo "/workspace/test" | md5sum | cut -c1-8)
hash2=$(echo "/workspace/test" | md5sum | cut -c1-8)
if [[ "$hash1" == "$hash2" ]]; then
    _test_pass "Workspace hash is consistent"
else
    _test_fail "Workspace hash is not consistent: $hash1 != $hash2"
fi

# Test 11: Different workspaces produce different hashes
_test_start "Different workspaces produce different hashes"
hash1=$(echo "/workspace/test1" | md5sum | cut -c1-8)
hash2=$(echo "/workspace/test2" | md5sum | cut -c1-8)
if [[ "$hash1" != "$hash2" ]]; then
    _test_pass "Different workspaces produce different hashes"
else
    _test_fail "Different workspaces produced same hash: $hash1"
fi

# Test 12: Config is read for rate limit interval
_test_start "Config is read for rate limit interval"
interval=$(get_config "knot.rate_limit_interval" "3600")
if [[ "$interval" == "3600" ]]; then
    _test_pass "Config rate limit interval read correctly"
else
    _test_fail "Config rate limit interval incorrect: expected 3600, got $interval"
fi

# Test 13: Pre-flight verification detects claimable beads via br ready
_test_start "Pre-flight detects claimable beads via br ready"
export MOCK_BR_READY_COUNT=5
if _needle_knot_verify_work_available "$TEST_WORKSPACE_DIR"; then
    _test_pass "Pre-flight correctly detected claimable beads"
else
    _test_fail "Pre-flight failed to detect claimable beads"
fi
unset MOCK_BR_READY_COUNT

# Test 14: Pre-flight returns no work when no claimable beads
_test_start "Pre-flight returns no work when no claimable beads"
export MOCK_BR_READY_COUNT=""
export MOCK_BR_LIST_DATA="[]"
if ! _needle_knot_verify_work_available "$TEST_WORKSPACE_DIR"; then
    _test_pass "Pre-flight correctly detected no claimable beads"
else
    _test_fail "Pre-flight incorrectly found claimable beads when none exist"
fi
unset MOCK_BR_READY_COUNT MOCK_BR_LIST_DATA

# Test 15: Strand skips alert when claimable beads exist (false positive prevention)
_test_start "Strand skips alert when claimable beads exist"
# Clear any rate limits first
_needle_knot_clear_rate_limit "$TEST_WORKSPACE_DIR"
# Set up mock to return claimable beads
export MOCK_BR_READY_COUNT=3
if ! _needle_strand_knot "$TEST_WORKSPACE_DIR" "test-agent"; then
    _test_pass "Strand correctly skipped alert when claimable beads exist"
else
    _test_fail "Strand incorrectly created alert despite claimable beads"
fi
unset MOCK_BR_READY_COUNT

# Test 16: Pre-flight detects claimable beads via br list fallback
_test_start "Pre-flight detects claimable beads via br list fallback"
# Mock br ready to fail (empty), but br list to have claimable beads
export MOCK_BR_READY_COUNT=""
export MOCK_BR_LIST_DATA='[{"id":"nd-test-1","title":"Test bead","status":"open","priority":2,"claimed_by":"","blocked_by":"","deferred_until":"","issue_type":"task"}]'
if _needle_knot_verify_work_available "$TEST_WORKSPACE_DIR"; then
    _test_pass "Pre-flight correctly used br list fallback"
else
    _test_fail "Pre-flight failed to use br list fallback"
fi
unset MOCK_BR_READY_COUNT MOCK_BR_LIST_DATA

# Test 17: Pre-flight excludes blocked beads from claimable count
_test_start "Pre-flight excludes blocked beads from claimable count"
export MOCK_BR_READY_COUNT=""
export MOCK_BR_LIST_DATA='[{"id":"nd-blocked-1","title":"Blocked bead","status":"open","priority":2,"claimed_by":"","blocked_by":"nd-other","deferred_until":"","issue_type":"task"}]'
if ! _needle_knot_verify_work_available "$TEST_WORKSPACE_DIR"; then
    _test_pass "Pre-flight correctly excluded blocked bead"
else
    _test_fail "Pre-flight incorrectly counted blocked bead as claimable"
fi
unset MOCK_BR_READY_COUNT MOCK_BR_LIST_DATA

# Test 18: Pre-flight excludes HUMAN beads from claimable count
_test_start "Pre-flight excludes HUMAN beads from claimable count"
export MOCK_BR_READY_COUNT=""
export MOCK_BR_LIST_DATA='[{"id":"nd-human-1","title":"Human alert","status":"open","priority":0,"claimed_by":"","blocked_by":"","deferred_until":"","issue_type":"human"}]'
if ! _needle_knot_verify_work_available "$TEST_WORKSPACE_DIR"; then
    _test_pass "Pre-flight correctly excluded HUMAN bead"
else
    _test_fail "Pre-flight incorrectly counted HUMAN bead as claimable"
fi
unset MOCK_BR_READY_COUNT MOCK_BR_LIST_DATA

# Test 19: Pre-flight excludes deferred beads from claimable count
_test_start "Pre-flight excludes deferred beads from claimable count"
export MOCK_BR_READY_COUNT=""
export MOCK_BR_LIST_DATA='[{"id":"nd-deferred-1","title":"Deferred bead","status":"open","priority":2,"claimed_by":"","blocked_by":"","deferred_until":"2026-12-31","issue_type":"task"}]'
if ! _needle_knot_verify_work_available "$TEST_WORKSPACE_DIR"; then
    _test_pass "Pre-flight correctly excluded deferred bead"
else
    _test_fail "Pre-flight incorrectly counted deferred bead as claimable"
fi
unset MOCK_BR_READY_COUNT MOCK_BR_LIST_DATA

# Summary
echo ""
echo "=========================================="
echo "Test Summary"
echo "=========================================="
echo "Passed: $TESTS_PASSED"
echo "Failed: $TESTS_FAILED"
echo ""

if [[ $TESTS_FAILED -eq 0 ]]; then
    echo "All tests passed!"
    exit 0
else
    echo "Some tests failed!"
    exit 1
fi
