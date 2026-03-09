#!/usr/bin/env bash
# Test suite for src/lib/errors.sh - Error handling standardization module
#
# Tests:
#   - Error type registry completeness and validity
#   - Exit code to event type mapping
#   - Escalation logic (retry/fail/quarantine)
#   - JSONL event validation (required fields, types)
#   - Error consistency across all strands
#   - Retry limit escalation

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
NEEDLE_HOME="$HOME/.needle-test-errors-$$"
NEEDLE_SESSION="test-session-errors"
NEEDLE_WORKSPACE="/tmp/test-workspace-errors"
NEEDLE_RUNNER="test-runner"
NEEDLE_PROVIDER="test-provider"
NEEDLE_MODEL="test-model"
NEEDLE_IDENTIFIER="test-identifier"
export NEEDLE_VERBOSE=false
export NEEDLE_DEFAULT_RETRY_COUNT=3

# Source events module first (errors.sh depends on it)
source "$PROJECT_ROOT/src/telemetry/events.sh"

# Source the errors module under test
source "$PROJECT_ROOT/src/lib/errors.sh"

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

# Emit event with verbose capture
_emit_capture() {
    NEEDLE_VERBOSE=true _needle_telemetry_emit "$@" 2>&1
}

cleanup() {
    rm -rf "$NEEDLE_HOME"
}
trap cleanup EXIT

# ============================================================================
# Test Cases
# ============================================================================

echo "=========================================="
echo "Running src/lib/errors.sh tests"
echo "=========================================="

# ----------------------------------------------------------------------------
# Test 1: Module loads without errors
# ----------------------------------------------------------------------------
_test_start "Module loads and exports expected functions"
functions_ok=true
for fn in \
    _needle_error_get_exit_code \
    _needle_error_get_escalation \
    _needle_error_is_registered \
    _needle_error_list_types \
    _needle_error_validate_jsonl_event \
    _needle_error_validate_error_event \
    _needle_error_escalation_with_retries \
    _needle_error_handle \
    _needle_error_handle_with_retries \
    _needle_error_audit_log; do
    if ! declare -f "$fn" &>/dev/null; then
        _test_fail "Function $fn not defined"
        functions_ok=false
    fi
done
$functions_ok && _test_pass "All expected functions are defined"

# ----------------------------------------------------------------------------
# Test 2: Error registry is populated
# ----------------------------------------------------------------------------
_test_start "Error registry is populated with entries"
registry_count="${#NEEDLE_ERROR_REGISTRY[@]}"
if [[ "$registry_count" -ge 10 ]]; then
    _test_pass "Error registry has $registry_count entries"
else
    _test_fail "Error registry too small: $registry_count entries"
fi

# ----------------------------------------------------------------------------
# Test 3: Registry entries have valid format (exit_code:escalation)
# ----------------------------------------------------------------------------
_test_start "Registry entries have valid format (exit_code:escalation)"
format_ok=true
for key in "${!NEEDLE_ERROR_REGISTRY[@]}"; do
    entry="${NEEDLE_ERROR_REGISTRY[$key]}"
    exit_code="${entry%%:*}"
    action="${entry##*:}"

    # Validate exit code is a number
    if ! [[ "$exit_code" =~ ^[0-9]+$ ]]; then
        _test_fail "Registry entry $key has non-numeric exit code: $exit_code"
        format_ok=false
    fi

    # Validate action is retry|fail|quarantine
    if ! [[ "$action" =~ ^(retry|fail|quarantine)$ ]]; then
        _test_fail "Registry entry $key has invalid action: $action"
        format_ok=false
    fi
done
$format_ok && _test_pass "All registry entries have valid format"

# ----------------------------------------------------------------------------
# Test 4: Registry event types are a subset of known event types
# ----------------------------------------------------------------------------
_test_start "Registry event types follow naming convention (category.action)"
naming_ok=true
for key in "${!NEEDLE_ERROR_REGISTRY[@]}"; do
    # All error event types should have a dot separator
    if ! [[ "$key" == *.* ]]; then
        _test_fail "Registry key $key does not follow category.action naming"
        naming_ok=false
    fi
done
$naming_ok && _test_pass "All registry keys follow category.action naming convention"

# ----------------------------------------------------------------------------
# Test 5: _needle_error_get_exit_code - known types
# ----------------------------------------------------------------------------
_test_start "_needle_error_get_exit_code returns correct exit codes"
exit_code_ok=true

# error.claim_failed -> 1
result=$(_needle_error_get_exit_code "error.claim_failed")
if [[ "$result" != "1" ]]; then
    _test_fail "error.claim_failed: expected exit code 1, got $result"
    exit_code_ok=false
fi

# error.agent_crash -> 137
result=$(_needle_error_get_exit_code "error.agent_crash")
if [[ "$result" != "137" ]]; then
    _test_fail "error.agent_crash: expected exit code 137, got $result"
    exit_code_ok=false
fi

# error.timeout -> 6
result=$(_needle_error_get_exit_code "error.timeout")
if [[ "$result" != "6" ]]; then
    _test_fail "error.timeout: expected exit code 6, got $result"
    exit_code_ok=false
fi

# Unknown type -> 1 (default)
result=$(_needle_error_get_exit_code "error.unknown_type_xyz")
if [[ "$result" != "1" ]]; then
    _test_fail "unknown type: expected default exit code 1, got $result"
    exit_code_ok=false
fi

$exit_code_ok && _test_pass "_needle_error_get_exit_code works correctly"

# ----------------------------------------------------------------------------
# Test 6: _needle_error_get_escalation - escalation actions
# ----------------------------------------------------------------------------
_test_start "_needle_error_get_escalation returns correct actions"
escalation_ok=true

# retry cases
for event in "error.claim_failed" "error.timeout" "error.rate_limited" "error.dependency_missing"; do
    result=$(_needle_error_get_escalation "$event")
    if [[ "$result" != "retry" ]]; then
        _test_fail "$event: expected retry, got $result"
        escalation_ok=false
    fi
done

# fail cases
for event in "bead.failed" "hook.failed" "error.config_invalid" "error.workspace_unavailable"; do
    result=$(_needle_error_get_escalation "$event")
    if [[ "$result" != "fail" ]]; then
        _test_fail "$event: expected fail, got $result"
        escalation_ok=false
    fi
done

# quarantine cases
for event in "error.agent_crash" "error.budget_exceeded" "error.budget_per_bead_exceeded"; do
    result=$(_needle_error_get_escalation "$event")
    if [[ "$result" != "quarantine" ]]; then
        _test_fail "$event: expected quarantine, got $result"
        escalation_ok=false
    fi
done

# Unknown type defaults to fail
result=$(_needle_error_get_escalation "error.nonexistent_type")
if [[ "$result" != "fail" ]]; then
    _test_fail "unknown type: expected default fail, got $result"
    escalation_ok=false
fi

$escalation_ok && _test_pass "_needle_error_get_escalation works correctly for all action types"

# ----------------------------------------------------------------------------
# Test 7: _needle_error_is_registered
# ----------------------------------------------------------------------------
_test_start "_needle_error_is_registered correctly identifies registered types"
if _needle_error_is_registered "error.claim_failed" && \
   _needle_error_is_registered "error.agent_crash" && \
   ! _needle_error_is_registered "error.not_a_real_event" && \
   ! _needle_error_is_registered ""; then
    _test_pass "_needle_error_is_registered works correctly"
else
    _test_fail "_needle_error_is_registered returned unexpected results"
fi

# ----------------------------------------------------------------------------
# Test 8: _needle_error_list_types returns all registered types
# ----------------------------------------------------------------------------
_test_start "_needle_error_list_types returns all registered event types"
listed_count=$(_needle_error_list_types | wc -l)
registry_count="${#NEEDLE_ERROR_REGISTRY[@]}"
if [[ "$listed_count" -eq "$registry_count" ]]; then
    _test_pass "_needle_error_list_types returns $listed_count types (matches registry)"
else
    _test_fail "listed $listed_count types but registry has $registry_count"
fi

# Verify specific entries are in the list
if _needle_error_list_types | grep -qx "error.claim_failed" && \
   _needle_error_list_types | grep -qx "error.agent_crash" && \
   _needle_error_list_types | grep -qx "error.timeout"; then
    _test_pass "Key error types are present in list"
else
    _test_fail "Some key error types are missing from list"
fi

# ----------------------------------------------------------------------------
# Test 9: JSONL event validation - valid event
# ----------------------------------------------------------------------------
_test_start "_needle_error_validate_jsonl_event accepts valid events"
valid_event='{"ts":"2026-03-01T10:00:00.123Z","event":"error.timeout","level":"error","session":"test-session","worker":"test-worker","data":{"operation":"claim"}}'
if _needle_error_validate_jsonl_event "$valid_event" 2>/dev/null; then
    _test_pass "Valid JSONL event accepted"
else
    _test_fail "Valid JSONL event rejected"
fi

# ----------------------------------------------------------------------------
# Test 10: JSONL event validation - missing required field
# ----------------------------------------------------------------------------
_test_start "_needle_error_validate_jsonl_event rejects events missing required fields"
validation_ok=true

# Missing ts
missing_ts='{"event":"error.timeout","level":"error","session":"s","worker":"w","data":{}}'
if _needle_error_validate_jsonl_event "$missing_ts" 2>/dev/null; then
    _test_fail "Event missing 'ts' field should be invalid"
    validation_ok=false
fi

# Missing event
missing_event='{"ts":"2026-03-01T10:00:00.123Z","level":"error","session":"s","worker":"w","data":{}}'
if _needle_error_validate_jsonl_event "$missing_event" 2>/dev/null; then
    _test_fail "Event missing 'event' field should be invalid"
    validation_ok=false
fi

# Missing level
missing_level='{"ts":"2026-03-01T10:00:00.123Z","event":"error.timeout","session":"s","worker":"w","data":{}}'
if _needle_error_validate_jsonl_event "$missing_level" 2>/dev/null; then
    _test_fail "Event missing 'level' field should be invalid"
    validation_ok=false
fi

# Empty string
if _needle_error_validate_jsonl_event "" 2>/dev/null; then
    _test_fail "Empty string should be invalid"
    validation_ok=false
fi

$validation_ok && _test_pass "Invalid events correctly rejected"

# ----------------------------------------------------------------------------
# Test 11: JSONL event validation - invalid level value
# ----------------------------------------------------------------------------
_test_start "_needle_error_validate_jsonl_event rejects invalid level values"
if command -v jq &>/dev/null; then
    bad_level='{"ts":"2026-03-01T10:00:00.123Z","event":"error.timeout","level":"critical","session":"s","worker":"w","data":{}}'
    if _needle_error_validate_jsonl_event "$bad_level" 2>/dev/null; then
        _test_fail "Event with invalid level 'critical' should be rejected"
    else
        _test_pass "Event with invalid level correctly rejected"
    fi
else
    _test_pass "Skipped (jq not available)"
fi

# ----------------------------------------------------------------------------
# Test 12: JSONL event validation - data must be an object
# ----------------------------------------------------------------------------
_test_start "_needle_error_validate_jsonl_event rejects non-object data field"
if command -v jq &>/dev/null; then
    string_data='{"ts":"2026-03-01T10:00:00.123Z","event":"error.timeout","level":"error","session":"s","worker":"w","data":"not-an-object"}'
    if _needle_error_validate_jsonl_event "$string_data" 2>/dev/null; then
        _test_fail "Event with string data field should be rejected"
    else
        _test_pass "Event with non-object data correctly rejected"
    fi
else
    _test_pass "Skipped (jq not available)"
fi

# ----------------------------------------------------------------------------
# Test 13: Escalation with retry limits
# ----------------------------------------------------------------------------
_test_start "_needle_error_escalation_with_retries respects retry limits"
retry_ok=true

# retry type with 0 retries -> retry
result=$(_needle_error_escalation_with_retries "error.claim_failed" 0 3)
if [[ "$result" != "retry" ]]; then
    _test_fail "0 retries with retry type: expected retry, got $result"
    retry_ok=false
fi

# retry type with 2 retries and max 3 -> retry
result=$(_needle_error_escalation_with_retries "error.claim_failed" 2 3)
if [[ "$result" != "retry" ]]; then
    _test_fail "2 retries (max 3) with retry type: expected retry, got $result"
    retry_ok=false
fi

# retry type with 3 retries (at max) -> fail
result=$(_needle_error_escalation_with_retries "error.claim_failed" 3 3)
if [[ "$result" != "fail" ]]; then
    _test_fail "3 retries at max (3): expected fail, got $result"
    retry_ok=false
fi

# retry type with 5 retries (over max 3) -> fail
result=$(_needle_error_escalation_with_retries "error.timeout" 5 3)
if [[ "$result" != "fail" ]]; then
    _test_fail "5 retries (max 3): expected fail, got $result"
    retry_ok=false
fi

# quarantine type stays quarantine regardless of retry count
result=$(_needle_error_escalation_with_retries "error.agent_crash" 0 3)
if [[ "$result" != "quarantine" ]]; then
    _test_fail "quarantine type should stay quarantine, got $result"
    retry_ok=false
fi

result=$(_needle_error_escalation_with_retries "error.agent_crash" 99 3)
if [[ "$result" != "quarantine" ]]; then
    _test_fail "quarantine type should stay quarantine at high retry count, got $result"
    retry_ok=false
fi

# fail type stays fail regardless of retry count
result=$(_needle_error_escalation_with_retries "bead.failed" 0 3)
if [[ "$result" != "fail" ]]; then
    _test_fail "fail type should stay fail, got $result"
    retry_ok=false
fi

$retry_ok && _test_pass "_needle_error_escalation_with_retries works correctly"

# ----------------------------------------------------------------------------
# Test 14: _needle_error_handle emits JSONL event and returns escalation
# ----------------------------------------------------------------------------
_test_start "_needle_error_handle emits valid JSONL event"
# Initialize log writer for event capture
_needle_init_log "$NEEDLE_SESSION"
export NEEDLE_VERBOSE=false

# Clear log and emit an error
: > "$NEEDLE_LOG_FILE"
action=$(_needle_error_handle "error.timeout" 6 "operation=claim_bead" "bead_id=test-123")

# Check action output
if [[ "$action" != "retry" ]]; then
    _test_fail "_needle_error_handle returned wrong action: $action (expected retry)"
else
    _test_pass "_needle_error_handle returned correct escalation action: $action"
fi

# Check event was logged
if grep -q '"event":"error.timeout"' "$NEEDLE_LOG_FILE" 2>/dev/null; then
    _test_pass "error.timeout event was written to log file"
else
    _test_fail "error.timeout event was not written to log file"
fi

# Validate the logged event structure
if command -v jq &>/dev/null; then
    logged_event=$(grep '"event":"error.timeout"' "$NEEDLE_LOG_FILE" | head -1)
    if _needle_error_validate_jsonl_event "$logged_event" 2>/dev/null; then
        _test_pass "Logged error event has valid JSONL structure"
    else
        _test_fail "Logged error event has invalid JSONL structure"
    fi

    # Check exit_code is in data
    exit_code_val=$(echo "$logged_event" | jq -r '.data.exit_code // ""' 2>/dev/null)
    if [[ "$exit_code_val" == "6" ]]; then
        _test_pass "Error event includes exit_code in data"
    else
        _test_fail "Error event missing exit_code in data: $exit_code_val"
    fi
fi

# ----------------------------------------------------------------------------
# Test 15: _needle_error_handle_with_retries
# ----------------------------------------------------------------------------
_test_start "_needle_error_handle_with_retries returns correct actions"
handle_retry_ok=true

: > "$NEEDLE_LOG_FILE"

# First attempt (retry_count=0) -> retry
action=$(_needle_error_handle_with_retries "error.claim_failed" 1 0)
if [[ "$action" != "retry" ]]; then
    _test_fail "retry_count=0: expected retry, got $action"
    handle_retry_ok=false
fi

# At max retries (retry_count=3, max=3) -> fail
action=$(_needle_error_handle_with_retries "error.claim_failed" 1 3)
if [[ "$action" != "fail" ]]; then
    _test_fail "retry_count=3 (at max): expected fail, got $action"
    handle_retry_ok=false
fi

# Quarantine type always quarantines
action=$(_needle_error_handle_with_retries "error.agent_crash" 137 0)
if [[ "$action" != "quarantine" ]]; then
    _test_fail "agent_crash: expected quarantine, got $action"
    handle_retry_ok=false
fi

# Check retry_count was logged in event data
if command -v jq &>/dev/null; then
    retry_count_val=$(grep '"event":"error.claim_failed"' "$NEEDLE_LOG_FILE" | head -1 | jq -r '.data.retry_count // ""' 2>/dev/null)
    if [[ -n "$retry_count_val" ]]; then
        _test_pass "retry_count is included in error event data"
    else
        _test_fail "retry_count not found in error event data"
        handle_retry_ok=false
    fi
fi

$handle_retry_ok && _test_pass "_needle_error_handle_with_retries works correctly"

# ----------------------------------------------------------------------------
# Test 16: _needle_error_audit_log validates log files
# ----------------------------------------------------------------------------
_test_start "_needle_error_audit_log passes for clean log files"

# Create a temp log with valid error events
temp_log=$(mktemp)
# Write a valid error event
echo '{"ts":"2026-03-01T10:00:00.123Z","event":"error.timeout","level":"error","session":"s","worker":"w","data":{"operation":"test"}}' >> "$temp_log"
echo '{"ts":"2026-03-01T10:00:01.000Z","event":"bead.claimed","level":"info","session":"s","worker":"w","data":{"bead_id":"test"}}' >> "$temp_log"

if _needle_error_audit_log "$temp_log" 2>/dev/null; then
    _test_pass "_needle_error_audit_log passes for valid log"
else
    _test_fail "_needle_error_audit_log failed for valid log"
fi
rm -f "$temp_log"

# ----------------------------------------------------------------------------
# Test 17: _needle_error_audit_log detects unregistered error events
# ----------------------------------------------------------------------------
_test_start "_needle_error_audit_log detects unregistered error event types"

temp_log=$(mktemp)
echo '{"ts":"2026-03-01T10:00:00.123Z","event":"error.unregistered_type_xyz","level":"error","session":"s","worker":"w","data":{}}' >> "$temp_log"

if _needle_error_audit_log "$temp_log" 2>/dev/null; then
    _test_fail "_needle_error_audit_log should fail for unregistered error type"
else
    _test_pass "_needle_error_audit_log correctly rejects unregistered error type"
fi
rm -f "$temp_log"

# ----------------------------------------------------------------------------
# Test 18: _needle_error_audit_log handles empty/non-existent files
# ----------------------------------------------------------------------------
_test_start "_needle_error_audit_log handles edge cases"
audit_edge_ok=true

# Non-existent file
if _needle_error_audit_log "/tmp/nonexistent-needle-log-xyz.jsonl" 2>/dev/null; then
    _test_fail "Should fail for non-existent log file"
    audit_edge_ok=false
fi

# Empty file
temp_log=$(mktemp)
if ! _needle_error_audit_log "$temp_log" 2>/dev/null; then
    _test_fail "Should pass for empty log file (no errors to validate)"
    audit_edge_ok=false
fi
rm -f "$temp_log"

$audit_edge_ok && _test_pass "_needle_error_audit_log handles edge cases correctly"

# ----------------------------------------------------------------------------
# Test 19: Error consistency - all strand-related error events are registered
# ----------------------------------------------------------------------------
_test_start "All strand-related error events are registered in the registry"
strand_ok=true

# Errors that should be emitted by pluck strand (claim failures, crashes, timeouts)
strand_errors=(
    "error.claim_failed"
    "error.agent_crash"
    "error.timeout"
    "bead.failed"
    "hook.failed"
)

for event in "${strand_errors[@]}"; do
    if ! _needle_error_is_registered "$event"; then
        _test_fail "Strand error event not registered: $event"
        strand_ok=false
    fi
done

# Errors that should be emitted by the runner/budget system
runner_errors=(
    "error.budget_exceeded"
    "error.budget_per_bead_exceeded"
    "error.rate_limited"
)

for event in "${runner_errors[@]}"; do
    if ! _needle_error_is_registered "$event"; then
        _test_fail "Runner error event not registered: $event"
        strand_ok=false
    fi
done

# Errors from the bead decomposition (mitosis) system
if ! _needle_error_is_registered "bead.mitosis.failed"; then
    _test_fail "bead.mitosis.failed not registered"
    strand_ok=false
fi

$strand_ok && _test_pass "All strand-related error events are registered"

# ----------------------------------------------------------------------------
# Test 20: Escalation action coverage - every action type is represented
# ----------------------------------------------------------------------------
_test_start "All three escalation actions (retry/fail/quarantine) are represented in registry"
has_retry=false
has_fail=false
has_quarantine=false

for key in "${!NEEDLE_ERROR_REGISTRY[@]}"; do
    action="${NEEDLE_ERROR_REGISTRY[$key]##*:}"
    case "$action" in
        retry)     has_retry=true ;;
        fail)      has_fail=true ;;
        quarantine) has_quarantine=true ;;
    esac
done

if $has_retry && $has_fail && $has_quarantine; then
    _test_pass "Registry contains entries for all three escalation actions"
else
    _test_fail "Registry missing some escalation actions (retry=$has_retry fail=$has_fail quarantine=$has_quarantine)"
fi

# ----------------------------------------------------------------------------
# Test 21: _needle_error_handle produces structured event with correct level
# ----------------------------------------------------------------------------
_test_start "_needle_error_handle always emits events at error level"
: > "$NEEDLE_LOG_FILE"

_needle_error_handle "error.claim_failed" 1 "bead_id=test-abc" > /dev/null

if command -v jq &>/dev/null; then
    level=$(grep '"event":"error.claim_failed"' "$NEEDLE_LOG_FILE" | head -1 | jq -r '.level' 2>/dev/null)
    if [[ "$level" == "error" ]]; then
        _test_pass "Error event emitted at 'error' level"
    else
        _test_fail "Error event not at 'error' level: $level"
    fi
else
    if grep -q '"level":"error"' "$NEEDLE_LOG_FILE" 2>/dev/null; then
        _test_pass "Error event emitted at 'error' level"
    else
        _test_fail "Error event not at 'error' level"
    fi
fi

# ----------------------------------------------------------------------------
# Test 22: Multiple concurrent errors handled independently
# ----------------------------------------------------------------------------
_test_start "Multiple error types handled independently and correctly"
: > "$NEEDLE_LOG_FILE"
multi_ok=true

action1=$(_needle_error_handle "error.claim_failed" 1 "bead_id=bead-1")
action2=$(_needle_error_handle "error.agent_crash" 137 "bead_id=bead-2")
action3=$(_needle_error_handle "bead.failed" 1 "bead_id=bead-3")

if [[ "$action1" != "retry" ]]; then
    _test_fail "error.claim_failed: expected retry, got $action1"
    multi_ok=false
fi
if [[ "$action2" != "quarantine" ]]; then
    _test_fail "error.agent_crash: expected quarantine, got $action2"
    multi_ok=false
fi
if [[ "$action3" != "fail" ]]; then
    _test_fail "bead.failed: expected fail, got $action3"
    multi_ok=false
fi

# Verify all three events were logged
event_count=$(grep -c '"level":"error"' "$NEEDLE_LOG_FILE" 2>/dev/null || echo 0)
if [[ "$event_count" -ge 3 ]]; then
    _test_pass "All error events logged independently ($event_count events)"
else
    _test_fail "Expected 3+ error events in log, found $event_count"
    multi_ok=false
fi

$multi_ok && _test_pass "Multiple error types handled independently and correctly"

# ============================================================================
# Summary
# ============================================================================

echo ""
echo "=========================================="
echo "Test Results"
echo "=========================================="
echo "Passed: $TESTS_PASSED"
echo "Failed: $TESTS_FAILED"
echo ""

if [[ $TESTS_FAILED -eq 0 ]]; then
    echo "✓ All tests passed!"
    exit 0
else
    echo "✗ Some tests failed"
    exit 1
fi
