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
# Disable auto-bead creation globally for tests to prevent side effects on the
# production workspace. Tests that specifically test auto-bead behavior must
# explicitly set NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="true".
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="false"

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

# ----------------------------------------------------------------------------
# Test 22a: error.dispatch_failed event function exists and works correctly
# ----------------------------------------------------------------------------
_test_start "_needle_event_error_dispatch_failed function is defined and works"
: > "$NEEDLE_LOG_FILE"

# Check function exists
if declare -f _needle_event_error_dispatch_failed &>/dev/null; then
    _test_pass "_needle_event_error_dispatch_failed function is defined"
else
    _test_fail "_needle_event_error_dispatch_failed function not found"
fi

# Test the function emits correct event
_needle_event_error_dispatch_failed "test-bead-123" \
    "agent=test-agent" \
    "error=could_not_start"

if grep -q '"event":"error.dispatch_failed"' "$NEEDLE_LOG_FILE" 2>/dev/null; then
    _test_pass "error.dispatch_failed event was written to log"
else
    _test_fail "error.dispatch_failed event was not written to log"
fi

# Verify it's NOT emitting error.agent_crash
if grep -q '"event":"error.agent_crash"' "$NEEDLE_LOG_FILE" 2>/dev/null; then
    _test_fail "dispatch_failed should NOT emit error.agent_crash event"
else
    _test_pass "dispatch_failed correctly does NOT emit error.agent_crash"
fi

# ----------------------------------------------------------------------------
# Test 22b: error.dispatch_failed is registered with correct escalation
# ----------------------------------------------------------------------------
_test_start "error.dispatch_failed has correct registry entry"
if _needle_error_is_registered "error.dispatch_failed"; then
    exit_code=$(_needle_error_get_exit_code "error.dispatch_failed")
    escalation=$(_needle_error_get_escalation "error.dispatch_failed")

    if [[ "$exit_code" == "4" ]] && [[ "$escalation" == "retry" ]]; then
        _test_pass "error.dispatch_failed: exit_code=4, escalation=retry"
    else
        _test_fail "error.dispatch_failed: expected exit_code=4, escalation=retry, got exit_code=$exit_code, escalation=$escalation"
    fi
else
    _test_fail "error.dispatch_failed not found in error registry"
fi

# ----------------------------------------------------------------------------
# Test 22c: error.agent_crash is specifically for exit code 137 (SIGKILL)
# ----------------------------------------------------------------------------
_test_start "error.agent_crash registry entry is specific to exit code 137"
if _needle_error_is_registered "error.agent_crash"; then
    exit_code=$(_needle_error_get_exit_code "error.agent_crash")
    escalation=$(_needle_error_get_escalation "error.agent_crash")

    if [[ "$exit_code" == "137" ]] && [[ "$escalation" == "quarantine" ]]; then
        _test_pass "error.agent_crash: exit_code=137, escalation=quarantine"
    else
        _test_fail "error.agent_crash: expected exit_code=137, escalation=quarantine, got exit_code=$exit_code, escalation=$escalation"
    fi
else
    _test_fail "error.agent_crash not found in error registry"
fi

# ============================================================================
# Auto Bug Bead Creation Tests
# ============================================================================

# Source config.sh for get_config function if not already available
if ! declare -f get_config &>/dev/null; then
    source "$PROJECT_ROOT/src/lib/config.sh"
fi

# ----------------------------------------------------------------------------
# Test 23: _needle_error_auto_bead function exists
# ----------------------------------------------------------------------------
_test_start "_needle_error_auto_bead function is defined"
if declare -f _needle_error_auto_bead &>/dev/null; then
    _test_pass "_needle_error_auto_bead function is defined"
else
    _test_fail "_needle_error_auto_bead function not found"
fi

# ----------------------------------------------------------------------------
# Test 24: Auto bead returns early when disabled
# ----------------------------------------------------------------------------
_test_start "_needle_error_auto_bead returns early when feature disabled"
# Ensure feature is disabled
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="false"
# Create a test workspace
TEST_WORKSPACE="/tmp/needle-test-auto-bead-$$"
mkdir -p "$TEST_WORKSPACE"
# Function should return 0 (success) without creating a bead
if _needle_error_auto_bead "error.test" "retry" "bead_id=test-123" 2>/dev/null; then
    _test_pass "Auto bead returns success when disabled"
else
    _test_fail "Auto bead should return 0 when disabled"
fi
rm -rf "$TEST_WORKSPACE"

# ----------------------------------------------------------------------------
# Test 25: Auto bead returns early when workspace not configured
# ----------------------------------------------------------------------------
_test_start "_needle_error_auto_bead returns early when workspace not configured"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="true"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_WORKSPACE=""
if _needle_error_auto_bead "error.test" "retry" 2>/dev/null; then
    _test_pass "Auto bead returns success when workspace not configured"
else
    _test_fail "Auto bead should return 0 when workspace not configured"
fi

# ----------------------------------------------------------------------------
# Test 26: Auto bead creates bead for quarantine escalation (with br mock)
# ----------------------------------------------------------------------------
_test_start "_needle_error_auto_bead attempts bead creation for quarantine errors"
# Create a mock br command that captures the call
TEST_WORKSPACE="/tmp/needle-test-ws-$$"
mkdir -p "$TEST_WORKSPACE/.beads"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="true"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_WORKSPACE="$TEST_WORKSPACE"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_RATE_LIMIT="0"

# Create a temporary br mock
MOCK_BR="/tmp/needle-mock-br-$$"
cat > "$MOCK_BR" <<'EOF'
#!/usr/bin/env bash
# Mock br CLI that simulates successful bead creation
if [[ "$1" == "create" ]]; then
    echo "nd-test-$(date +%s)"
    exit 0
fi
exit 1
EOF
chmod +x "$MOCK_BR"

# Temporarily add mock to PATH
export PATH="/tmp:$PATH"
ln -sf "$MOCK_BR" "/tmp/br"

# Clear any existing rate limit state
STATE_DIR="$NEEDLE_HOME/$NEEDLE_STATE_DIR"
rm -f "$STATE_DIR/auto_bead_signatures.json" 2>/dev/null
mkdir -p "$STATE_DIR"

# Call auto bead with quarantine escalation
if _needle_error_auto_bead "error.test_quarantine" "quarantine" "bead_id=test-456" 2>/dev/null; then
    _test_pass "Auto bead function returns success for quarantine error"
else
    _test_fail "Auto bead should return 0 for quarantine error"
fi

# Cleanup
rm -f "/tmp/br" "$MOCK_BR"
rm -rf "$TEST_WORKSPACE"

# ----------------------------------------------------------------------------
# Test 27: Auto bead creates bead for unregistered error types
# ----------------------------------------------------------------------------
_test_start "_needle_error_auto_bead handles unregistered error types"
TEST_WORKSPACE="/tmp/needle-test-ws2-$$"
mkdir -p "$TEST_WORKSPACE/.beads"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="true"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_WORKSPACE="$TEST_WORKSPACE"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_RATE_LIMIT="0"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_TYPES="unregistered"

# Verify error.type_not_registered is not in registry
if _needle_error_is_registered "error.type_not_registered"; then
    _test_fail "Test setup error: error.type_not_registered should not be registered"
else
    _test_pass "Test error type correctly unregistered"
fi

# Create mock br
MOCK_BR2="/tmp/needle-mock-br2-$$"
cat > "$MOCK_BR2" <<'EOF'
#!/usr/bin/env bash
if [[ "$1" == "create" ]]; then
    echo "nd-unreg-$(date +%s)"
    exit 0
fi
exit 1
EOF
chmod +x "$MOCK_BR2"
ln -sf "$MOCK_BR2" "/tmp/br"

# Call auto bead with unregistered error type
if _needle_error_auto_bead "error.type_not_registered" "fail" 2>/dev/null; then
    _test_pass "Auto bead function returns success for unregistered error type"
else
    _test_fail "Auto bead should return 0 for unregistered error type"
fi

# Cleanup
rm -f "/tmp/br" "$MOCK_BR2"
rm -rf "$TEST_WORKSPACE"

# ----------------------------------------------------------------------------
# Test 28: Auto bead rate limiting prevents duplicate filings
# ----------------------------------------------------------------------------
_test_start "Auto bead rate limiting uses signature-based deduplication"
TEST_WORKSPACE="/tmp/needle-test-ws3-$$"
mkdir -p "$TEST_WORKSPACE/.beads"

# Create a test config file
TEST_CONFIG="/tmp/needle-test-config-$$"
cat > "$TEST_CONFIG" <<'EOF'
debug:
  auto_bead_on_error: true
  auto_bead_workspace: PLACEHOLDER
  auto_bead_rate_limit: 3600
EOF

# Replace workspace placeholder
sed -i "s|PLACEHOLDER|$TEST_WORKSPACE|g" "$TEST_CONFIG"

# Export config path for this test
export NEEDLE_CONFIG_FILE="$TEST_CONFIG"

# Create mock br
MOCK_BR3="/tmp/needle-mock-br3-$$"
cat > "$MOCK_BR3" <<'EOF'
#!/usr/bin/env bash
if [[ "$1" == "create" ]]; then
    echo "nd-rate-$(date +%s)"
    exit 0
fi
exit 1
EOF
chmod +x "$MOCK_BR3"
ln -sf "$MOCK_BR3" "/tmp/br"

# Clear state and create first bead
STATE_DIR="$NEEDLE_HOME/$NEEDLE_STATE_DIR"
rm -f "$STATE_DIR/auto_bead_signatures.json" 2>/dev/null
mkdir -p "$STATE_DIR"

# First call should succeed (or attempt creation)
_needle_error_auto_bead "error.rate_test" "quarantine" 2>/dev/null

# Second call should also succeed (defensive - returns early if rate limited)
if _needle_error_auto_bead "error.rate_test" "quarantine" 2>/dev/null; then
    _test_pass "Rate limited calls return success"
else
    _test_fail "Rate limited call should return 0"
fi

# The signature file mechanism is internal and uses python3
# If python3 is available, the file should exist after calls
if command -v python3 &>/dev/null && [[ -f "$STATE_DIR/auto_bead_signatures.json" ]]; then
    _test_pass "Signature state file created (python3 available)"
elif ! command -v python3 &>/dev/null; then
    _test_pass "Signature file skipped (python3 not available - expected)"
fi

# Cleanup
rm -f "/tmp/br" "$MOCK_BR3" "$TEST_CONFIG"
rm -rf "$TEST_WORKSPACE"
rm -f "$STATE_DIR/auto_bead_signatures.json"

# ----------------------------------------------------------------------------
# Test 29: Auto bead respects auto_bead_types configuration
# ----------------------------------------------------------------------------
_test_start "Auto bead respects auto_bead_types configuration"
TEST_WORKSPACE="/tmp/needle-test-ws4-$$"
mkdir -p "$TEST_WORKSPACE/.beads"

# Test with only quarantine enabled
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="true"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_WORKSPACE="$TEST_WORKSPACE"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_RATE_LIMIT="0"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_TYPES="quarantine"

# Create mock br
MOCK_BR4="/tmp/needle-mock-br4-$$"
cat > "$MOCK_BR4" <<'EOF'
#!/usr/bin/env bash
if [[ "$1" == "create" ]]; then
    echo "nd-types-$(date +%s)"
    exit 0
fi
exit 1
EOF
chmod +x "$MOCK_BR4"
ln -sf "$MOCK_BR4" "/tmp/br"

# Clear state
rm -f "$STATE_DIR/auto_bead_signatures.json" 2>/dev/null

# Quarantine should create bead
if _needle_error_auto_bead "error.test_quarantine2" "quarantine" 2>/dev/null; then
    _test_pass "Quarantine type creates bead when in auto_bead_types"
else
    _test_fail "Quarantine should create bead when enabled"
fi

# Cleanup
rm -f "/tmp/br" "$MOCK_BR4"
rm -rf "$TEST_WORKSPACE"

# ----------------------------------------------------------------------------
# Test 30: _needle_error_handle calls auto_bead for quarantine errors
# ----------------------------------------------------------------------------
_test_start "_needle_error_handle integrates with auto_bead for quarantine"
: > "$NEEDLE_LOG_FILE"

TEST_WORKSPACE="/tmp/needle-test-ws5-$$"
mkdir -p "$TEST_WORKSPACE/.beads"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="true"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_WORKSPACE="$TEST_WORKSPACE"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_RATE_LIMIT="0"

# Create mock br
MOCK_BR5="/tmp/needle-mock-br5-$$"
cat > "$MOCK_BR5" <<'EOF'
#!/usr/bin/env bash
if [[ "$1" == "create" ]]; then
    echo "nd-integrated-$(date +%s)"
    exit 0
fi
exit 1
EOF
chmod +x "$MOCK_BR5"
ln -sf "$MOCK_BR5" "/tmp/br"

# Clear state
rm -f "$STATE_DIR/auto_bead_signatures.json" 2>/dev/null

# _needle_error_handle should call auto_bead internally for quarantine
action=$(_needle_error_handle "error.agent_crash" 137 "bead_id=nd-integrated-test")
if [[ "$action" == "quarantine" ]]; then
    _test_pass "_needle_error_handle returns quarantine for agent_crash"
else
    _test_fail "_needle_error_handle should return quarantine, got $action"
fi

# Cleanup
rm -f "/tmp/br" "$MOCK_BR5"
rm -rf "$TEST_WORKSPACE"
rm -f "$STATE_DIR/auto_bead_signatures.json"

# ----------------------------------------------------------------------------
# Test 31: Auto bead log file lookup uses correct path (NEEDLE_SESSION)
# ----------------------------------------------------------------------------
_test_start "_needle_error_auto_bead uses NEEDLE_SESSION for log file lookup"
TEST_WORKSPACE="/tmp/needle-test-ws6-$$"
mkdir -p "$TEST_WORKSPACE/.beads"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="true"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_WORKSPACE="$TEST_WORKSPACE"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_RATE_LIMIT="0"

# Set up session and worker ID to match real worker behavior
# In production, logs are named ${NEEDLE_SESSION}.log, not ${NEEDLE_WORKER_ID}.log
# NOTE: session name must NOT match test-*, needle-test-*, or perf-* patterns,
# as those trigger the test session guard and prevent auto-bead creation.
export NEEDLE_SESSION="needle-worker-agent-alpha"
export NEEDLE_WORKER_ID="alpha"
export NEEDLE_LOG_DIR="logs"
# Unset NEEDLE_LOG_FILE so the function uses NEEDLE_LOG_DIR path construction
# (NEEDLE_LOG_FILE may have been set by _needle_init_log earlier in the test suite)
unset NEEDLE_LOG_FILE

# Create the log file with session name (as worker does)
mkdir -p "$NEEDLE_HOME/logs"
echo "Test log line 1" > "$NEEDLE_HOME/logs/${NEEDLE_SESSION}.log"
echo "Test log line 2" >> "$NEEDLE_HOME/logs/${NEEDLE_SESSION}.log"
echo "Test log line 3" >> "$NEEDLE_HOME/logs/${NEEDLE_SESSION}.log"

# Create mock br that captures the description
MOCK_BR6="/tmp/needle-mock-br6-$$"
DESC_FILE="/tmp/needle-test-desc-$$"
cat > "$MOCK_BR6" <<BRSCRIPT
#!/usr/bin/env bash
# Export the PID so the subshell can find the temp file
export TEST_DESC_PID="\$\$"

if [[ "\$1" == "create" ]]; then
    # Capture all arguments to reconstruct the description
    # br create uses --description with multi-line content
    desc_capture=""
    in_desc=false

    for arg in "\$@"; do
        if [[ "\$arg" == "--description" ]]; then
            in_desc=true
            continue
        fi
        if [[ "\$arg" == --* ]] && [[ "\$in_desc" == "true" ]]; then
            in_desc=false
        fi
        if [[ "\$in_desc" == "true" ]]; then
            desc_capture+="\$arg"\n
        fi
    done

    # Write captured description to temp file
    printf '%s' "\$desc_capture" > "$DESC_FILE" 2>/dev/null
    echo "nd-log-test-\$(date +%s)"
    exit 0
fi
exit 1
BRSCRIPT
chmod +x "$MOCK_BR6"

# Temporarily add mock to PATH
export PATH="/tmp:$PATH"
ln -sf "$MOCK_BR6" "/tmp/br"

# Clear state
rm -f "$STATE_DIR/auto_bead_signatures.json" 2>/dev/null
rm -f "$DESC_FILE"

# Call auto bead with quarantine escalation
if _needle_error_auto_bead "error.test_log_lookup" "quarantine" "bead_id=test-log-lookup" 2>/dev/null; then
    _test_pass "Auto bead function returns success"

    # Verify the log content was included (check description file)
    if [[ -f "$DESC_FILE" ]]; then
        if grep -q "Test log line" "$DESC_FILE" 2>/dev/null; then
            _test_pass "Log file content found in auto bead description using NEEDLE_SESSION"
        else
            _test_fail "Log content not found in auto bead description"
        fi

        # Verify it's NOT using worker_id.log path
        if grep -q "alpha.log" "$DESC_FILE" 2>/dev/null; then
            _test_fail "Description incorrectly references worker_id.log path"
        else
            _test_pass "Description does not incorrectly reference worker_id.log"
        fi
    else
        _test_fail "Description file not created by mock br"
    fi
else
    _test_fail "Auto bead should return 0 for quarantine error"
fi

# Cleanup
rm -f "/tmp/br" "$MOCK_BR6" "$DESC_FILE" "$NEEDLE_HOME/logs/${NEEDLE_SESSION}.log"
rm -rf "$TEST_WORKSPACE"

# ----------------------------------------------------------------------------
# Test 32: Auto bead log file lookup with full path NEEDLE_LOG_DIR (runner export)
# ----------------------------------------------------------------------------
# This tests the fix for bug nd-6ys3 where NEEDLE_LOG_DIR is exported as a full
# path by the runner (e.g., /home/user/.needle/logs) but the auto_bead function
# was treating it as a relative directory name, resulting in malformed paths.
_test_start "_needle_error_auto_bead handles full path NEEDLE_LOG_DIR correctly"

TEST_WORKSPACE="/tmp/needle-test-ws7-$$"
mkdir -p "$TEST_WORKSPACE/.beads"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="true"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_WORKSPACE="$TEST_WORKSPACE"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_RATE_LIMIT="0"

# Set up session
# NOTE: session name must NOT match test-*, needle-test-*, or perf-* patterns,
# as those trigger the test session guard and prevent auto-bead creation.
export NEEDLE_SESSION="needle-worker-agent-fullpath"
export NEEDLE_WORKER_ID="bravo"
# Unset NEEDLE_LOG_FILE so the function uses NEEDLE_LOG_DIR path construction
unset NEEDLE_LOG_FILE

# Export NEEDLE_LOG_DIR as a full path (as the runner does)
export NEEDLE_LOG_DIR="$NEEDLE_HOME/logs"
export NEEDLE_HOME="$NEEDLE_HOME"

# Create the log file at the full path
mkdir -p "$NEEDLE_LOG_DIR"
echo "Full path test log line 1" > "$NEEDLE_LOG_DIR/${NEEDLE_SESSION}.log"
echo "Full path test log line 2" >> "$NEEDLE_LOG_DIR/${NEEDLE_SESSION}.log"

# Create mock br that captures the description
MOCK_BR7="/tmp/needle-mock-br7-$$"
DESC_FILE="/tmp/needle-test-desc-fullpath-$$"
cat > "$MOCK_BR7" <<BRSCRIPT
#!/usr/bin/env bash
if [[ "\$1" == "create" ]]; then
    desc_capture=""
    in_desc=false
    for arg in "\$@"; do
        if [[ "\$arg" == "--description" ]]; then
            in_desc=true
            continue
        fi
        if [[ "\$arg" == --* ]] && [[ "\$in_desc" == "true" ]]; then
            in_desc=false
        fi
        if [[ "\$in_desc" == "true" ]]; then
            desc_capture+="\$arg"\n
        fi
    done
    printf '%s' "\$desc_capture" > "$DESC_FILE" 2>/dev/null
    echo "nd-fullpath-test-\$(date +%s)"
    exit 0
fi
exit 1
BRSCRIPT
chmod +x "$MOCK_BR7"
ln -sf "$MOCK_BR7" "/tmp/br"

# Clear state
rm -f "$STATE_DIR/auto_bead_signatures.json" 2>/dev/null
rm -f "$DESC_FILE"

# Call auto bead with quarantine escalation
if _needle_error_auto_bead "error.test_fullpath_log" "quarantine" "bead_id=test-fullpath" 2>/dev/null; then
    _test_pass "Auto bead function returns success with full path NEEDLE_LOG_DIR"

    # Verify the log content was included
    if [[ -f "$DESC_FILE" ]]; then
        if grep -q "Full path test log line" "$DESC_FILE" 2>/dev/null; then
            _test_pass "Log file content found when NEEDLE_LOG_DIR is a full path"
        else
            _test_fail "Log content not found - full path handling may be broken"
        fi

        # Verify we don't see the malformed double-slash path pattern
        if grep -q "//" "$DESC_FILE" 2>/dev/null; then
            _test_fail "Malformed double-slash path found in description"
        else
            _test_pass "No malformed double-slash paths in description"
        fi
    else
        _test_fail "Description file not created by mock br"
    fi
else
    _test_fail "Auto bead should return 0 for quarantine error with full path NEEDLE_LOG_DIR"
fi

# Cleanup
rm -f "/tmp/br" "$MOCK_BR7" "$DESC_FILE" "$NEEDLE_LOG_DIR/${NEEDLE_SESSION}.log"
rm -rf "$TEST_WORKSPACE"

# ----------------------------------------------------------------------------
# Test 33: _needle_error_handle doesn't create real beads when auto-bead is
# disabled (nd-lbtu regression)
# ----------------------------------------------------------------------------
# Bug: test 22 called _needle_error_handle "error.agent_crash" 137 without
# disabling auto-bead, causing it to read the production config
# (~/.needle/config.yaml) and create a real bead in the production workspace.
_test_start "_needle_error_handle with error.agent_crash doesn't create beads when auto-bead disabled"

# Ensure auto-bead is disabled (as set by the global default above)
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="false"

# Set up a mock br that records if it was called
BR_CALLED_LBTU="/tmp/needle-br-called-lbtu-$$"
MOCK_BR_LBTU="/tmp/needle-mock-br-lbtu-$$"
rm -f "$BR_CALLED_LBTU"
cat > "$MOCK_BR_LBTU" <<EOF
#!/usr/bin/env bash
echo "called" > "$BR_CALLED_LBTU"
echo "nd-should-not-exist-lbtu"
exit 0
EOF
chmod +x "$MOCK_BR_LBTU"
ln -sf "$MOCK_BR_LBTU" "/tmp/br"
export PATH="/tmp:$PATH"

# Replicate the multi-error test scenario from Test 22 that triggered the bug
action1=$(_needle_error_handle "error.claim_failed" 1 "bead_id=bead-1")
action2=$(_needle_error_handle "error.agent_crash" 137 "bead_id=bead-2")

# Verify escalation is still correct
if [[ "$action1" == "retry" ]] && [[ "$action2" == "quarantine" ]]; then
    _test_pass "Escalation actions are correct (retry/quarantine)"
else
    _test_fail "Unexpected escalation: claim_failed=$action1, agent_crash=$action2"
fi

# Verify no bead was created (br should not have been invoked)
if [[ -f "$BR_CALLED_LBTU" ]]; then
    _test_fail "br was called when auto-bead is disabled - production workspace contaminated (nd-lbtu regression)"
else
    _test_pass "No bead created when auto-bead is disabled (nd-lbtu regression)"
fi

# Cleanup
rm -f "/tmp/br" "$MOCK_BR_LBTU" "$BR_CALLED_LBTU"

# ----------------------------------------------------------------------------
# Test 34: Auto-bead skips creation for test sessions (nd-wgbf, nd-hjdz regression)
# ----------------------------------------------------------------------------
# Bug: test sessions with names like "test-session-errors" could trigger
# auto-bead creation if the config override wasn't properly applied (e.g.,
# when running specific test functions in isolation or via IDE execution).
# Both nd-wgbf and nd-hjdz were auto-created from the same test run before
# the test session safeguard was added.
# This test verifies that test sessions are ALWAYS skipped regardless of config.
_test_start "_needle_error_auto_bead skips for test sessions even when enabled"

# Temporarily enable auto-bead (simulating production config)
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="true"

# Create test workspace
TEST_WORKSPACE_WGBF="/tmp/needle-test-ws-wgbf-$$"
mkdir -p "$TEST_WORKSPACE_WGBF/.beads"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_WORKSPACE="$TEST_WORKSPACE_WGBF"
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_RATE_LIMIT="0"

# Create a mock br that records if it was called
BR_CALLED_WGBF="/tmp/needle-br-called-wgbf-$$"
MOCK_BR_WGBF="/tmp/needle-mock-br-wgbf-$$"
rm -f "$BR_CALLED_WGBF"
cat > "$MOCK_BR_WGBF" <<EOF
#!/usr/bin/env bash
echo "called" > "$BR_CALLED_WGBF"
echo "nd-should-not-exist-wgbf"
exit 0
EOF
chmod +x "$MOCK_BR_WGBF"
ln -sf "$MOCK_BR_WGBF" "/tmp/br"
export PATH="/tmp:$PATH"

# Test with various test session patterns
for test_session in "test-session-errors" "test-foo" "needle-test-alpha" "perf-benchmark"; do
    export NEEDLE_SESSION="$test_session"

    # Clear any previous call marker
    rm -f "$BR_CALLED_WGBF"

    # Call auto_bead with quarantine escalation
    _needle_error_auto_bead "error.agent_crash" "quarantine" "bead_id=test-wgbf" 2>/dev/null

    if [[ -f "$BR_CALLED_WGBF" ]]; then
        _test_fail "br was called for test session '$test_session' - test session safeguard failed"
        break
    fi
done

# Only pass if none of the test sessions triggered bead creation
if [[ ! -f "$BR_CALLED_WGBF" ]]; then
    _test_pass "Auto-bead skipped for all test session patterns (nd-wgbf regression)"
fi

# Verify that production sessions still work
export NEEDLE_SESSION="needle-worker-prod"
rm -f "$BR_CALLED_WGBF"
_needle_error_auto_bead "error.agent_crash" "quarantine" "bead_id=test-prod" 2>/dev/null

if [[ -f "$BR_CALLED_WGBF" ]]; then
    _test_pass "Auto-bead works for production sessions"
else
    _test_fail "Auto-bead should work for production sessions"
fi

# Cleanup
rm -f "/tmp/br" "$MOCK_BR_WGBF" "$BR_CALLED_WGBF"
rm -rf "$TEST_WORKSPACE_WGBF"

# Reset to disabled for remaining tests
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="false"

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
