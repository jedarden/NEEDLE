#!/usr/bin/env bash
# Test script for telemetry/events.sh module

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
source "$PROJECT_ROOT/src/telemetry/writer.sh"

# Set up test environment
NEEDLE_HOME="$HOME/.needle-test-events-$$"
NEEDLE_SESSION="test-session-events-$$"
NEEDLE_WORKSPACE="/tmp/test-workspace-events"
NEEDLE_AGENT="test-agent"
NEEDLE_RUNNER="test-runner"
NEEDLE_PROVIDER="test-provider"
NEEDLE_MODEL="test-model"
NEEDLE_IDENTIFIER="test-identifier"
NEEDLE_VERBOSE=false

# Source the events module after setting environment
source "$PROJECT_ROOT/src/telemetry/events.sh"

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

# Cleanup function
cleanup() {
    rm -rf "$NEEDLE_HOME"
}
trap cleanup EXIT

# ============================================================================
# Test Cases
# ============================================================================

echo "=========================================="
echo "Running telemetry/events.sh tests"
echo "=========================================="

# Test 1: Worker JSON generation
_test_start "Worker JSON generation"
worker_json=$(_needle_telemetry_worker_json)
if echo "$worker_json" | jq -e '.runner == "test-runner"' > /dev/null 2>&1 && \
   echo "$worker_json" | jq -e '.provider == "test-provider"' > /dev/null 2>&1 && \
   echo "$worker_json" | jq -e '.model == "test-model"' > /dev/null 2>&1 && \
   echo "$worker_json" | jq -e '.identifier == "test-identifier"' > /dev/null 2>&1; then
    _test_pass "Worker JSON contains all required fields"
else
    _test_fail "Worker JSON missing required fields" "$worker_json"
fi

# Test 2: Timestamp generation
_test_start "Timestamp generation (ISO8601 format)"
ts=$(_needle_telemetry_timestamp)
# Match pattern like 2026-03-01T10:00:00.123Z or 2026-03-01T10:00:00.000Z
if [[ "$ts" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{3}Z$ ]]; then
    _test_pass "Timestamp is ISO8601 with milliseconds: $ts"
else
    _test_fail "Timestamp format incorrect" "$ts"
fi

# Test 3: Build data object with various types
_test_start "Build data object with various types"
data=$(_needle_telemetry_build_data "string_val=hello" "num_val=42" "bool_val=true" "null_val=null")
if echo "$data" | jq -e '.string_val == "hello"' > /dev/null 2>&1 && \
   echo "$data" | jq -e '.num_val == 42' > /dev/null 2>&1 && \
   echo "$data" | jq -e '.bool_val == true' > /dev/null 2>&1 && \
   echo "$data" | jq -e '.null_val == null' > /dev/null 2>&1; then
    _test_pass "Data object handles all value types"
else
    _test_fail "Data object type handling failed" "$data"
fi

# Test 4: Emit basic event to stdout
_test_start "Emit basic event to stdout"
# Use NEEDLE_VERBOSE=true in the same shell as the emit call
output=$(NEEDLE_VERBOSE=true _needle_telemetry_emit "test.event" "key1=value1" "key2=value2")

if echo "$output" | jq -e '.event == "test.event"' > /dev/null 2>&1 && \
   echo "$output" | jq -e '.data.key1 == "value1"' > /dev/null 2>&1; then
    _test_pass "Basic event emitted correctly"
else
    _test_fail "Basic event emission failed" "$output"
fi

# Test 5: Event envelope structure
_test_start "Event envelope structure"
output=$(_needle_telemetry_emit "test.envelope" "test=value")
has_ts=$(echo "$output" | jq 'has("ts")')
has_event=$(echo "$output" | jq 'has("event")')
has_session=$(echo "$output" | jq 'has("session")')
has_worker=$(echo "$output" | jq 'has("worker")')
has_data=$(echo "$output" | jq 'has("data")')

if [[ "$has_ts" == "true" && "$has_event" == "true" && "$has_session" == "true" && \
      "$has_worker" == "true" && "$has_data" == "true" ]]; then
    _test_pass "Event envelope has all required fields"
else
    _test_fail "Event envelope missing fields"
fi

# Test 6: Worker events
_test_start "Worker events (started, idle, stopped, draining)"
events_ok=true

# Test worker.started
output=$(_needle_telemetry_emit "worker.started" "pid=123")
if ! echo "$output" | jq -e '.event == "worker.started"' > /dev/null 2>&1; then
    events_ok=false
fi

# Test worker.idle
output=$(_needle_telemetry_emit "worker.idle")
if ! echo "$output" | jq -e '.event == "worker.idle"' > /dev/null 2>&1; then
    events_ok=false
fi

# Test worker.stopped
output=$(_needle_telemetry_emit "worker.stopped" "reason=test")
if ! echo "$output" | jq -e '.event == "worker.stopped"' > /dev/null 2>&1; then
    events_ok=false
fi

# Test worker.draining
output=$(_needle_telemetry_emit "worker.draining")
if ! echo "$output" | jq -e '.event == "worker.draining"' > /dev/null 2>&1; then
    events_ok=false
fi

if $events_ok; then
    _test_pass "All worker events emit correctly"
else
    _test_fail "Some worker events failed"
fi

# Test 7: Bead events
_test_start "Bead events (claimed, completed, failed, released)"
bead_events_ok=true

# Test bead.claimed
output=$(_needle_telemetry_emit "bead.claimed" "bead_id=test-123")
if ! echo "$output" | jq -e '.event == "bead.claimed"' > /dev/null 2>&1 || \
   ! echo "$output" | jq -e '.data.bead_id == "test-123"' > /dev/null 2>&1; then
    bead_events_ok=false
fi

# Test bead.completed
output=$(_needle_telemetry_emit "bead.completed" "bead_id=test-123" "result=success")
if ! echo "$output" | jq -e '.event == "bead.completed"' > /dev/null 2>&1; then
    bead_events_ok=false
fi

# Test bead.failed
output=$(_needle_telemetry_emit "bead.failed" "bead_id=test-123" "error=test_error")
if ! echo "$output" | jq -e '.event == "bead.failed"' > /dev/null 2>&1; then
    bead_events_ok=false
fi

if $bead_events_ok; then
    _test_pass "All bead events emit correctly"
else
    _test_fail "Some bead events failed"
fi

# Test 8: Strand events
_test_start "Strand events (started, completed, skipped)"
strand_events_ok=true

output=$(_needle_telemetry_emit "strand.started" "bead_id=test-123" "strand=1")
if ! echo "$output" | jq -e '.event == "strand.started"' > /dev/null 2>&1 || \
   ! echo "$output" | jq -e '.data.strand == 1' > /dev/null 2>&1; then
    strand_events_ok=false
fi

output=$(_needle_telemetry_emit "strand.completed" "bead_id=test-123" "strand=1")
if ! echo "$output" | jq -e '.event == "strand.completed"' > /dev/null 2>&1; then
    strand_events_ok=false
fi

if $strand_events_ok; then
    _test_pass "All strand events emit correctly"
else
    _test_fail "Some strand events failed"
fi

# Test 9: Hook events
_test_start "Hook events (started, completed, failed)"
hook_events_ok=true

output=$(_needle_telemetry_emit "hook.started" "hook=pre_exec")
if ! echo "$output" | jq -e '.event == "hook.started"' > /dev/null 2>&1; then
    hook_events_ok=false
fi

output=$(_needle_telemetry_emit "hook.completed" "hook=pre_exec" "duration_ms=100")
if ! echo "$output" | jq -e '.event == "hook.completed"' > /dev/null 2>&1; then
    hook_events_ok=false
fi

output=$(_needle_telemetry_emit "hook.failed" "hook=pre_exec" "error=test")
if ! echo "$output" | jq -e '.event == "hook.failed"' > /dev/null 2>&1; then
    hook_events_ok=false
fi

if $hook_events_ok; then
    _test_pass "All hook events emit correctly"
else
    _test_fail "Some hook events failed"
fi

# Test 10: Heartbeat events
_test_start "Heartbeat events (emitted, stuck_detected, recovery)"
hb_events_ok=true

output=$(_needle_telemetry_emit "heartbeat.emitted" "status=idle")
if ! echo "$output" | jq -e '.event == "heartbeat.emitted"' > /dev/null 2>&1; then
    hb_events_ok=false
fi

output=$(_needle_telemetry_emit "heartbeat.stuck_detected" "stuck_session=worker-123")
if ! echo "$output" | jq -e '.event == "heartbeat.stuck_detected"' > /dev/null 2>&1; then
    hb_events_ok=false
fi

output=$(_needle_telemetry_emit "heartbeat.recovery" "recovered_session=worker-123")
if ! echo "$output" | jq -e '.event == "heartbeat.recovery"' > /dev/null 2>&1; then
    hb_events_ok=false
fi

if $hb_events_ok; then
    _test_pass "All heartbeat events emit correctly"
else
    _test_fail "Some heartbeat events failed"
fi

# Test 11: Error events
_test_start "Error events (claim_failed, agent_crash, timeout)"
error_events_ok=true

output=$(_needle_telemetry_emit "error.claim_failed" "bead_id=test-123" "reason=locked")
if ! echo "$output" | jq -e '.event == "error.claim_failed"' > /dev/null 2>&1; then
    error_events_ok=false
fi

output=$(_needle_telemetry_emit "error.agent_crash" "agent=test-agent" "error=segfault")
if ! echo "$output" | jq -e '.event == "error.agent_crash"' > /dev/null 2>&1; then
    error_events_ok=false
fi

output=$(_needle_telemetry_emit "error.timeout" "operation=claim" "duration_seconds=30")
if ! echo "$output" | jq -e '.event == "error.timeout"' > /dev/null 2>&1; then
    error_events_ok=false
fi

if $error_events_ok; then
    _test_pass "All error events emit correctly"
else
    _test_fail "Some error events failed"
fi

# Test 12: Event type validation
_test_start "Event type validation"
if _needle_telemetry_valid_event "worker.started" && \
   _needle_telemetry_valid_event "bead.claimed" && \
   _needle_telemetry_valid_event "error.timeout" && \
   ! _needle_telemetry_valid_event "invalid.event.type"; then
    _test_pass "Event type validation works correctly"
else
    _test_fail "Event type validation failed"
fi

# Test 13: Convenience functions
_test_start "Convenience functions for event emission"
convenience_ok=true

# Test _needle_event_bead_claimed
output=$(_needle_event_bead_claimed "bead-123" "workspace=/test")
if ! echo "$output" | jq -e '.event == "bead.claimed"' > /dev/null 2>&1 || \
   ! echo "$output" | jq -e '.data.bead_id == "bead-123"' > /dev/null 2>&1; then
    convenience_ok=false
fi

# Test _needle_event_worker_started
output=$(_needle_event_worker_started)
if ! echo "$output" | jq -e '.event == "worker.started"' > /dev/null 2>&1; then
    convenience_ok=false
fi

# Test _needle_event_bead_completed
output=$(_needle_event_bead_completed "bead-456" "result=success")
if ! echo "$output" | jq -e '.event == "bead.completed"' > /dev/null 2>&1; then
    convenience_ok=false
fi

if $convenience_ok; then
    _test_pass "Convenience functions work correctly"
else
    _test_fail "Some convenience functions failed"
fi

# Test 14: Write to log file
_test_start "Write to log file"
# Initialize log
_needle_init_log "$NEEDLE_SESSION"
NEEDLE_VERBOSE=false

# Emit event
_needle_telemetry_emit "test.log.write" "testing=file_write"

# Check if written to log
if grep -q '"event":"test.log.write"' "$NEEDLE_LOG_FILE" 2>/dev/null; then
    _test_pass "Event written to log file"
else
    _test_fail "Event not written to log file" "Log file: $NEEDLE_LOG_FILE"
fi

# Test 15: Special character handling
_test_start "Special character handling in data values"
output=$(_needle_telemetry_emit "test.special" "message=hello \"world\"" "path=/tmp/test file.txt")
if echo "$output" | jq -e '.data.message == "hello \"world\""' > /dev/null 2>&1; then
    _test_pass "Special characters handled correctly"
else
    _test_fail "Special character handling failed" "$output"
fi

# Test 16: Negative number handling
_test_start "Negative number handling"
output=$(_needle_telemetry_emit "test.numbers" "positive=42" "negative=-10" "float=3.14")
if echo "$output" | jq -e '.data.positive == 42' > /dev/null 2>&1 && \
   echo "$output" | jq -e '.data.negative == -10' > /dev/null 2>&1 && \
   echo "$output" | jq -e '.data.float == 3.14' > /dev/null 2>&1; then
    _test_pass "Number handling works correctly"
else
    _test_fail "Number handling failed" "$output"
fi

# Test 17: Event types listing
_test_start "Event types listing"
event_count=$(_needle_telemetry_event_types | wc -l)
if [[ "$event_count" -ge 20 ]]; then
    _test_pass "Event types list contains $event_count events"
else
    _test_fail "Event types list too short: $event_count events"
fi

# Test 18: Multiple events to log file
_test_start "Multiple events appended to log file"
# Clear log file
: > "$NEEDLE_LOG_FILE"

# Emit multiple events
_needle_telemetry_emit "test.multi.1" "seq=1"
_needle_telemetry_emit "test.multi.2" "seq=2"
_needle_telemetry_emit "test.multi.3" "seq=3"

line_count=$(wc -l < "$NEEDLE_LOG_FILE")
if [[ "$line_count" -eq 3 ]]; then
    _test_pass "Multiple events appended correctly"
else
    _test_fail "Expected 3 lines, got $line_count"
fi

# Test 19: Strand event convenience functions
_test_start "Strand event convenience functions"
strand_conv_ok=true

output=$(_needle_event_strand_started "bead-abc" "5")
if ! echo "$output" | jq -e '.data.strand == 5' > /dev/null 2>&1; then
    strand_conv_ok=false
fi

output=$(_needle_event_strand_completed "bead-abc" "5" "result=done")
if ! echo "$output" | jq -e '.data.strand == 5' > /dev/null 2>&1; then
    strand_conv_ok=false
fi

if $strand_conv_ok; then
    _test_pass "Strand convenience functions work"
else
    _test_fail "Strand convenience functions failed"
fi

# Test 20: Error event convenience functions
_test_start "Error event convenience functions"
error_conv_ok=true

output=$(_needle_event_error_claim_failed "bead-xyz" "reason=locked")
if ! echo "$output" | jq -e '.event == "error.claim_failed"' > /dev/null 2>&1; then
    error_conv_ok=false
fi

output=$(_needle_event_error_timeout "claim_bead" "duration_seconds=60")
if ! echo "$output" | jq -e '.data.operation == "claim_bead"' > /dev/null 2>&1; then
    error_conv_ok=false
fi

if $error_conv_ok; then
    _test_pass "Error convenience functions work"
else
    _test_fail "Error convenience functions failed"
fi

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
