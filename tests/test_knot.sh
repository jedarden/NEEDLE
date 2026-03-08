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

# Test 20: Verification diagnostics function emits event (nd-1xl)
_test_start "Verification diagnostics function emits event"
# Capture output from the diagnostics function (NEEDLE_VERBOSE=true outputs to stdout)
output=$(_needle_knot_emit_verification_diagnostics "/test/workspace" \
    "br_ready=0" \
    "needle_ready=0" \
    "direct_query=0" \
    "any_open=5" \
    "claimed=2" \
    "blocked=1" \
    "deferred=0" \
    "human_type=1" \
    "has_deps=1" 2>&1)

# Check that event was emitted (appears in stdout when NEEDLE_VERBOSE=true)
if echo "$output" | grep -q "knot.verification_diagnostic"; then
    _test_pass "Verification diagnostics event was emitted"
else
    _test_fail "Verification diagnostics event was not emitted"
fi

# Test 21: Verification diagnostics includes all checked values (nd-1xl)
_test_start "Verification diagnostics includes all checked values"

# Call the diagnostics function with specific values
output=$(_needle_knot_emit_verification_diagnostics "/test/workspace" \
    "br_ready=3" \
    "needle_ready=2" \
    "direct_query=1" \
    "any_open=10" \
    "claimed=5" \
    "blocked=2" \
    "deferred=1" \
    "human_type=1" \
    "has_deps=3" 2>&1)

# Check that all values are in the emitted event
all_values_present=true
for value in "br_ready.*3" "needle_ready.*2" "direct_query.*1" "any_open.*10" "claimed.*5" "blocked.*2" "deferred.*1" "human_type.*1" "has_deps.*3"; do
    if ! echo "$output" | grep -q "$value"; then
        all_values_present=false
        break
    fi
done

if [[ "$all_values_present" == "true" ]]; then
    _test_pass "Verification diagnostics includes all checked values"
else
    _test_fail "Verification diagnostics missing some values"
fi

# Test 22: Pre-flight emits verification diagnostics when no work found (nd-1xl)
_test_start "Pre-flight emits verification diagnostics when no work found"
export MOCK_BR_READY_COUNT=""
export MOCK_BR_LIST_DATA='[{"id":"nd-claimed-1","title":"Claimed bead","status":"open","priority":2,"claimed_by":"other-agent","blocked_by":"","deferred_until":"","issue_type":"task"}]'

# Capture output from verification
output=$(_needle_knot_verify_work_available "$TEST_WORKSPACE_DIR" 2>&1)
result=$?

# Check that verification found no claimable beads and emitted diagnostics
if [[ $result -ne 0 ]]; then
    # Check that diagnostic event was emitted
    if echo "$output" | grep -q "knot.verification_diagnostic"; then
        _test_pass "Pre-flight emitted verification diagnostics when no work found"
    else
        _test_fail "Pre-flight did not emit verification diagnostics"
    fi
else
    _test_fail "Pre-flight incorrectly found claimable beads"
fi
unset MOCK_BR_READY_COUNT MOCK_BR_LIST_DATA

# Test 23: Pre-flight does NOT emit diagnostics when work IS available (nd-1xl)
_test_start "Pre-flight does NOT emit diagnostics when work IS available"
export MOCK_BR_READY_COUNT=5

# Capture output from verification (should find claimable beads)
output=$(_needle_knot_verify_work_available "$TEST_WORKSPACE_DIR" 2>&1)
result=$?

if [[ $result -eq 0 ]]; then
    # Check that diagnostic event was NOT emitted (since work was found)
    if ! echo "$output" | grep -q "knot.verification_diagnostic"; then
        _test_pass "Pre-flight did not emit diagnostics when work available"
    else
        _test_fail "Pre-flight incorrectly emitted diagnostics when work found"
    fi
else
    _test_fail "Pre-flight failed to find claimable beads"
fi
unset MOCK_BR_READY_COUNT

# Test 24: Pre-flight skips alert when ALL beads are assigned to ACTIVE workers (nd-ane)
_test_start "Pre-flight skips alert when ALL beads assigned to active workers (nd-ane)"
export MOCK_BR_READY_COUNT=""
export MOCK_BR_LIST_DATA='[{"id":"nd-assigned-1","title":"Assigned bead 1","status":"open","priority":2,"claimed_by":"","blocked_by":"","deferred_until":"","issue_type":"task","assignee":"worker-alpha"},{"id":"nd-assigned-2","title":"Assigned bead 2","status":"open","priority":1,"claimed_by":"","blocked_by":"","deferred_until":"","issue_type":"task","assignee":"worker-beta"}]'

# Create fresh heartbeat files for both assigned workers
HEARTBEAT_DIR="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
mkdir -p "$HEARTBEAT_DIR"
NOW_TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
echo "{\"worker\":\"worker-alpha\",\"pid\":1001,\"last_heartbeat\":\"$NOW_TS\",\"status\":\"executing\"}" > "$HEARTBEAT_DIR/worker-alpha.json"
echo "{\"worker\":\"worker-beta\",\"pid\":1002,\"last_heartbeat\":\"$NOW_TS\",\"status\":\"executing\"}" > "$HEARTBEAT_DIR/worker-beta.json"

# Capture output from verification
output=$(_needle_knot_verify_work_available "$TEST_WORKSPACE_DIR" 2>&1)
result=$?

# Should return 1 (no work for THIS worker) but emit "all_work_assigned" event
if [[ $result -ne 0 ]]; then
    # Check that "all work assigned" event was emitted
    if echo "$output" | grep -q "knot.all_work_assigned"; then
        _test_pass "Pre-flight correctly detected all work assigned to active workers"
    else
        _test_fail "Pre-flight did not emit all_work_assigned event" "$(echo "$output" | tail -5)"
    fi
else
    _test_fail "Pre-flight incorrectly found claimable beads when all are assigned"
fi
rm -f "$HEARTBEAT_DIR/worker-alpha.json" "$HEARTBEAT_DIR/worker-beta.json"
unset MOCK_BR_READY_COUNT MOCK_BR_LIST_DATA

# Test 25: Pre-flight creates alert when SOME beads are unassigned (nd-ane)
_test_start "Pre-flight creates alert when SOME beads are unassigned (nd-ane)"
export MOCK_BR_READY_COUNT=""
export MOCK_BR_LIST_DATA='[{"id":"nd-assigned-1","title":"Assigned bead","status":"open","priority":2,"claimed_by":"","blocked_by":"","deferred_until":"","issue_type":"task","assignee":"worker-alpha"},{"id":"nd-unassigned-1","title":"Unassigned bead","status":"open","priority":1,"claimed_by":"other","blocked_by":"","deferred_until":"","issue_type":"task","assignee":""}]'

# Capture output from verification
output=$(_needle_knot_verify_work_available "$TEST_WORKSPACE_DIR" 2>&1)
result=$?

# Should return 1 (no work) and NOT emit "all_work_assigned" since not all are assigned
if [[ $result -ne 0 ]]; then
    # Check that "all work assigned" event was NOT emitted
    if ! echo "$output" | grep -q "knot.all_work_assigned"; then
        _test_pass "Pre-flight correctly did not emit all_work_assigned when some unassigned"
    else
        _test_fail "Pre-flight incorrectly emitted all_work_assigned when some beads unassigned"
    fi
else
    _test_fail "Pre-flight incorrectly found claimable beads"
fi
unset MOCK_BR_READY_COUNT MOCK_BR_LIST_DATA

# Test 26: Pre-flight proceeds with alert when assigned workers are STALE (nd-ane)
_test_start "Pre-flight proceeds with alert when assigned workers are stale (nd-ane)"
export MOCK_BR_READY_COUNT=""
export MOCK_BR_LIST_DATA='[{"id":"nd-assigned-1","title":"Assigned bead 1","status":"open","priority":2,"claimed_by":"","blocked_by":"","deferred_until":"","issue_type":"task","assignee":"worker-stale-alpha"},{"id":"nd-assigned-2","title":"Assigned bead 2","status":"open","priority":1,"claimed_by":"","blocked_by":"","deferred_until":"","issue_type":"task","assignee":"worker-stale-beta"}]'

# Create STALE heartbeat files (timestamp 10 minutes ago, well past 120s timeout)
HEARTBEAT_DIR="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
mkdir -p "$HEARTBEAT_DIR"
STALE_TS=$(date -u -d '10 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-10M +%Y-%m-%dT%H:%M:%SZ)
echo "{\"worker\":\"worker-stale-alpha\",\"pid\":2001,\"last_heartbeat\":\"$STALE_TS\",\"status\":\"executing\"}" > "$HEARTBEAT_DIR/worker-stale-alpha.json"
echo "{\"worker\":\"worker-stale-beta\",\"pid\":2002,\"last_heartbeat\":\"$STALE_TS\",\"status\":\"executing\"}" > "$HEARTBEAT_DIR/worker-stale-beta.json"

# Capture output from verification
output=$(_needle_knot_verify_work_available "$TEST_WORKSPACE_DIR" 2>&1)
result=$?

# Should return 1 (no work) and emit "assigned_workers_stale" event (NOT all_work_assigned)
if [[ $result -ne 0 ]]; then
    if echo "$output" | grep -q "knot.assigned_workers_stale"; then
        _test_pass "Pre-flight correctly detected stale workers and will proceed with alert"
    elif echo "$output" | grep -q "knot.all_work_assigned"; then
        _test_fail "Pre-flight incorrectly skipped alert for stale workers"
    else
        _test_fail "Pre-flight did not emit expected event" "$(echo "$output" | tail -5)"
    fi
else
    _test_fail "Pre-flight incorrectly found claimable beads"
fi
rm -f "$HEARTBEAT_DIR/worker-stale-alpha.json" "$HEARTBEAT_DIR/worker-stale-beta.json"
unset MOCK_BR_READY_COUNT MOCK_BR_LIST_DATA

# Test 27: Pre-flight proceeds with alert when assigned workers have NO heartbeat (nd-ane)
_test_start "Pre-flight proceeds with alert when assigned workers have no heartbeat (nd-ane)"
export MOCK_BR_READY_COUNT=""
export MOCK_BR_LIST_DATA='[{"id":"nd-assigned-1","title":"Assigned bead","status":"open","priority":2,"claimed_by":"","blocked_by":"","deferred_until":"","issue_type":"task","assignee":"worker-ghost"}]'

# No heartbeat file for worker-ghost
rm -f "$HEARTBEAT_DIR/worker-ghost.json"

# Capture output from verification
output=$(_needle_knot_verify_work_available "$TEST_WORKSPACE_DIR" 2>&1)
result=$?

if [[ $result -ne 0 ]]; then
    if echo "$output" | grep -q "knot.assigned_workers_stale"; then
        _test_pass "Pre-flight correctly detected missing heartbeat and will proceed with alert"
    elif echo "$output" | grep -q "knot.all_work_assigned"; then
        _test_fail "Pre-flight incorrectly skipped alert for worker with no heartbeat"
    else
        _test_fail "Pre-flight did not emit expected event" "$(echo "$output" | tail -5)"
    fi
else
    _test_fail "Pre-flight incorrectly found claimable beads"
fi
unset MOCK_BR_READY_COUNT MOCK_BR_LIST_DATA

# Test 28: _needle_knot_is_worker_active function tests (nd-ane)
_test_start "Worker active check returns true for fresh heartbeat (nd-ane)"
HEARTBEAT_DIR="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
mkdir -p "$HEARTBEAT_DIR"
NOW_TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
echo "{\"worker\":\"test-active-worker\",\"pid\":9001,\"last_heartbeat\":\"$NOW_TS\",\"status\":\"idle\"}" > "$HEARTBEAT_DIR/test-active-worker.json"
if _needle_knot_is_worker_active "test-active-worker" "$HEARTBEAT_DIR"; then
    _test_pass "Worker correctly detected as active"
else
    _test_fail "Worker with fresh heartbeat should be active"
fi
rm -f "$HEARTBEAT_DIR/test-active-worker.json"

# Test 29: Worker active check returns false for stale heartbeat (nd-ane)
_test_start "Worker active check returns false for stale heartbeat (nd-ane)"
STALE_TS=$(date -u -d '10 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-10M +%Y-%m-%dT%H:%M:%SZ)
echo "{\"worker\":\"test-stale-worker\",\"pid\":9002,\"last_heartbeat\":\"$STALE_TS\",\"status\":\"idle\"}" > "$HEARTBEAT_DIR/test-stale-worker.json"
if ! _needle_knot_is_worker_active "test-stale-worker" "$HEARTBEAT_DIR"; then
    _test_pass "Worker correctly detected as stale"
else
    _test_fail "Worker with stale heartbeat should be inactive"
fi
rm -f "$HEARTBEAT_DIR/test-stale-worker.json"

# Test 30: Worker active check returns false for missing heartbeat (nd-ane)
_test_start "Worker active check returns false for missing heartbeat (nd-ane)"
if ! _needle_knot_is_worker_active "nonexistent-worker" "$HEARTBEAT_DIR"; then
    _test_pass "Worker correctly detected as inactive (no heartbeat file)"
else
    _test_fail "Worker with no heartbeat should be inactive"
fi

# Test 31: Double-check function detects available work (nd-kon)
_test_start "Double-check function detects available work (nd-kon)"
export MOCK_BR_READY_COUNT=5
if _needle_knot_double_check_work_available "$TEST_WORKSPACE_DIR"; then
    _test_pass "Double-check correctly detected available work"
else
    _test_fail "Double-check failed to detect available work"
fi
unset MOCK_BR_READY_COUNT

# Test 32: Double-check function returns no work when none available (nd-kon)
_test_start "Double-check function returns no work when none available (nd-kon)"
export MOCK_BR_READY_COUNT=""
if ! _needle_knot_double_check_work_available "$TEST_WORKSPACE_DIR"; then
    _test_pass "Double-check correctly detected no available work"
else
    _test_fail "Double-check incorrectly found available work when none exists"
fi
unset MOCK_BR_READY_COUNT

# Test 33: Alert creation skips when double-check finds work (nd-kon)
_test_start "Alert creation skips when double-check finds work (nd-kon)"
# Clear rate limits first
_needle_knot_clear_rate_limit "$TEST_WORKSPACE_DIR"
# Set up mock to simulate: pre-flight passes (no work), but double-check finds work
# This simulates a race condition where work becomes available between checks
export MOCK_BR_READY_COUNT=3

# Call create_alert directly - it should skip because double-check finds work
output=$(_needle_knot_create_alert "$TEST_WORKSPACE_DIR" "test-agent" 2>&1)
result=$?

# Should return 1 (alert NOT created) and emit stale_alert_prevented event
if [[ $result -ne 0 ]]; then
    if echo "$output" | grep -q "knot.stale_alert_prevented"; then
        _test_pass "Alert creation correctly skipped when double-check found work"
    else
        _test_fail "Alert creation skipped but did not emit stale_alert_prevented event"
    fi
else
    _test_fail "Alert creation should have been skipped when work available"
fi
unset MOCK_BR_READY_COUNT

# Test 34: Alert creation proceeds when double-check confirms no work (nd-kon)
_test_start "Alert creation proceeds when double-check confirms no work (nd-kon)"
# Clear rate limits first
_needle_knot_clear_rate_limit "$TEST_WORKSPACE_DIR"
# Set up mock to simulate: both pre-flight and double-check find no work
export MOCK_BR_READY_COUNT=""

# Call create_alert directly - it should proceed and create the alert
output=$(_needle_knot_create_alert "$TEST_WORKSPACE_DIR" "test-agent" 2>&1)
result=$?

# Should return 0 (alert created)
if [[ $result -eq 0 ]]; then
    _test_pass "Alert creation correctly proceeded when no work available"
else
    _test_fail "Alert creation should have succeeded when no work available"
fi
unset MOCK_BR_READY_COUNT

# Test 35: DB health check returns healthy when no script exists (nd-1jv)
_test_start "DB health check returns healthy when no script exists (nd-1jv)"
if _needle_knot_check_db_health "/nonexistent/workspace"; then
    _test_pass "DB health check correctly returned healthy when no script exists"
else
    _test_fail "DB health check should return healthy when script is missing"
fi

# Test 36: DB health check returns healthy when script exits 0 (nd-1jv)
_test_start "DB health check returns healthy when script exits 0 (nd-1jv)"
MOCK_HEALTH_DIR="$TEST_WORKSPACE_DIR/.beads/maintenance"
mkdir -p "$MOCK_HEALTH_DIR"
cat > "$MOCK_HEALTH_DIR/db-health-check.sh" << 'SCRIPT'
#!/bin/bash
echo "✅ Database healthy"
exit 0
SCRIPT
chmod +x "$MOCK_HEALTH_DIR/db-health-check.sh"
if _needle_knot_check_db_health "$TEST_WORKSPACE_DIR"; then
    _test_pass "DB health check correctly returned healthy (exit 0)"
else
    _test_fail "DB health check should return healthy when script exits 0"
fi

# Test 37: DB health check returns corruption when script exits 1 (nd-1jv)
_test_start "DB health check returns corruption when script exits 1 (nd-1jv)"
cat > "$MOCK_HEALTH_DIR/db-health-check.sh" << 'SCRIPT'
#!/bin/bash
echo "🚨 CRITICAL: WAL file exceeds 10MB"
echo "✅ Database rebuilt successfully from JSONL"
exit 1
SCRIPT
chmod +x "$MOCK_HEALTH_DIR/db-health-check.sh"
if ! _needle_knot_check_db_health "$TEST_WORKSPACE_DIR"; then
    _test_pass "DB health check correctly returned corruption (exit 1)"
else
    _test_fail "DB health check should return corruption when script exits 1"
fi

# Test 38: DB health check treats exit 2 as healthy (error case) (nd-1jv)
_test_start "DB health check treats exit 2 as healthy (error case) (nd-1jv)"
cat > "$MOCK_HEALTH_DIR/db-health-check.sh" << 'SCRIPT'
#!/bin/bash
echo "❌ ERROR: rebuild failed"
exit 2
SCRIPT
chmod +x "$MOCK_HEALTH_DIR/db-health-check.sh"
if _needle_knot_check_db_health "$TEST_WORKSPACE_DIR"; then
    _test_pass "DB health check correctly treated error as healthy (don't suppress alerts)"
else
    _test_fail "DB health check should treat exit 2 as healthy to avoid suppressing alerts"
fi

# Test 39: Strand skips alert after DB corruption fix reveals work (nd-1jv)
_test_start "Strand skips alert after DB corruption fix reveals work (nd-1jv)"
_needle_knot_clear_rate_limit "$TEST_WORKSPACE_DIR"
MOCK_HEALTH_DIR="$TEST_WORKSPACE_DIR/.beads/maintenance"
mkdir -p "$MOCK_HEALTH_DIR"
# DB health check reports corruption (exit 1) AND creates a flag file
# that the br mock uses to switch behavior (simulating rebuilt DB)
DB_REBUILT_FLAG="/tmp/test_knot_db_rebuilt_$$"
export DB_REBUILT_FLAG
rm -f "$DB_REBUILT_FLAG"
cat > "$MOCK_HEALTH_DIR/db-health-check.sh" << SCRIPT
#!/bin/bash
touch "$DB_REBUILT_FLAG"
exit 1
SCRIPT
chmod +x "$MOCK_HEALTH_DIR/db-health-check.sh"

# Override br: returns no work unless DB has been rebuilt
br() {
    case "\$1" in
        ready)
            if [[ -f "$DB_REBUILT_FLAG" ]]; then
                echo '[{"id":"nd-recovered-1","title":"Recovered bead","priority":2}]'
            else
                echo '[]'
            fi
            ;;
        list)
            echo '[]'
            ;;
        create)
            echo "nd-knot-test-$$"
            return 0
            ;;
        *)
            return 0
            ;;
    esac
}
export -f br

output=$(_needle_strand_knot "$TEST_WORKSPACE_DIR" "test-agent" 2>&1)
result=$?

# Should return 1 (no alert created) because work was found after DB rebuild
if [[ $result -ne 0 ]]; then
    _test_pass "Strand correctly skipped alert after DB corruption fix"
else
    _test_fail "Strand should not have created alert when DB corruption caused false starvation"
fi
rm -f "$DB_REBUILT_FLAG"
unset DB_REBUILT_FLAG

# Restore default br mock
br() {
    case "$1" in
        ready)
            if [[ -n "$MOCK_BR_READY_COUNT" ]]; then
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
            echo "nd-knot-test-$$"
            return 0
            ;;
        *)
            return 0
            ;;
    esac
}
export -f br
unset MOCK_BR_READY_COUNT MOCK_BR_LIST_DATA

# Clean up mock health check
rm -rf "$TEST_WORKSPACE_DIR/.beads/maintenance"

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
