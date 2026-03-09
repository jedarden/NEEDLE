#!/usr/bin/env bash
# Tests for NEEDLE logs CLI command (src/cli/logs.sh)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

# Test counters
TESTS_PASSED=0
TESTS_FAILED=0

pass() {
    printf "${GREEN}PASS${NC} %s\n" "$1"
    TESTS_PASSED=$((TESTS_PASSED + 1))
}

fail() {
    printf "${RED}FAIL${NC} %s\n" "$1"
    TESTS_FAILED=$((TESTS_FAILED + 1))
}

# ============================================================================
# Set up test environment
# ============================================================================

TEST_DIR=$(mktemp -d)
TEST_NEEDLE_HOME="$TEST_DIR/.needle"
TEST_LOG_DIR="$TEST_NEEDLE_HOME/logs"

cleanup() {
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

mkdir -p "$TEST_LOG_DIR"

export NEEDLE_HOME="$TEST_NEEDLE_HOME"
export NEEDLE_QUIET=true
export NEEDLE_USE_COLOR=false

# Source required modules
source "$PROJECT_ROOT/src/lib/constants.sh"
source "$PROJECT_ROOT/src/lib/output.sh"
source "$PROJECT_ROOT/src/lib/utils.sh"
source "$PROJECT_ROOT/src/cli/logs.sh"

# ============================================================================
# Create test log files
# ============================================================================

# Worker alpha log
cat > "$TEST_LOG_DIR/needle-alpha.jsonl" << 'EOF'
{"ts":"2024-01-15T10:00:00Z","event":"worker.started","session":"needle-alpha","data":{"version":"0.1.0"}}
{"ts":"2024-01-15T10:01:00Z","event":"bead.claimed","session":"needle-alpha","data":{"bead_id":"nd-abc1","strand":1}}
{"ts":"2024-01-15T10:02:00Z","event":"bead.completed","session":"needle-alpha","data":{"bead_id":"nd-abc1","strand":1}}
{"ts":"2024-01-15T10:03:00Z","event":"heartbeat.ping","session":"needle-alpha","data":{}}
{"ts":"2024-01-15T10:04:00Z","event":"bead.failed","session":"needle-alpha","data":{"bead_id":"nd-abc2","error":"timeout"}}
EOF

# Worker bravo log
cat > "$TEST_LOG_DIR/needle-bravo.jsonl" << 'EOF'
{"ts":"2024-01-15T09:00:00Z","event":"worker.started","session":"needle-bravo","data":{"version":"0.1.0"}}
{"ts":"2024-01-15T09:30:00Z","event":"bead.claimed","session":"needle-bravo","data":{"bead_id":"nd-def1","strand":2}}
{"ts":"2024-01-15T09:45:00Z","event":"strand.fallthrough","session":"needle-bravo","data":{"strand":2}}
{"ts":"2024-01-15T09:50:00Z","event":"worker.stopped","session":"needle-bravo","data":{}}
EOF

echo "Running logs CLI tests..."
echo ""

# ============================================================================
# Test: Help Output
# ============================================================================

echo "=== Help Tests ==="

HELP_OUTPUT=$(_needle_logs_help 2>&1 || true)

if echo "$HELP_OUTPUT" | grep -q "USAGE:"; then
    pass "_needle_logs_help shows USAGE section"
else
    fail "_needle_logs_help missing USAGE section"
fi

if echo "$HELP_OUTPUT" | grep -q "\-\-follow"; then
    pass "_needle_logs_help shows --follow option"
else
    fail "_needle_logs_help missing --follow option"
fi

if echo "$HELP_OUTPUT" | grep -q "\-\-json"; then
    pass "_needle_logs_help shows --json option"
else
    fail "_needle_logs_help missing --json option"
fi

if echo "$HELP_OUTPUT" | grep -q "\-\-since"; then
    pass "_needle_logs_help shows --since option"
else
    fail "_needle_logs_help missing --since option"
fi

if echo "$HELP_OUTPUT" | grep -q "\-\-until"; then
    pass "_needle_logs_help shows --until option"
else
    fail "_needle_logs_help missing --until option"
fi

if echo "$HELP_OUTPUT" | grep -q "\-\-event"; then
    pass "_needle_logs_help shows --event option"
else
    fail "_needle_logs_help missing --event option"
fi

if echo "$HELP_OUTPUT" | grep -q "\-\-bead"; then
    pass "_needle_logs_help shows --bead option"
else
    fail "_needle_logs_help missing --bead option"
fi

if echo "$HELP_OUTPUT" | grep -q "needle logs"; then
    pass "_needle_logs_help shows example usage"
else
    fail "_needle_logs_help missing example usage"
fi

echo ""

# ============================================================================
# Test: Source File Structure
# ============================================================================

echo "=== Source File Tests ==="

if [[ -f "$PROJECT_ROOT/src/cli/logs.sh" ]]; then
    pass "logs.sh source file exists"
else
    fail "logs.sh source file missing"
fi

if grep -q "_needle_logs\b" "$PROJECT_ROOT/src/cli/logs.sh" 2>/dev/null; then
    pass "logs.sh has _needle_logs function"
else
    fail "logs.sh missing _needle_logs function"
fi

if grep -q "_needle_logs_help" "$PROJECT_ROOT/src/cli/logs.sh" 2>/dev/null; then
    pass "logs.sh has _needle_logs_help function"
else
    fail "logs.sh missing _needle_logs_help function"
fi

if grep -q "_needle_format_log_line" "$PROJECT_ROOT/src/cli/logs.sh" 2>/dev/null; then
    pass "logs.sh has _needle_format_log_line function"
else
    fail "logs.sh missing _needle_format_log_line function"
fi

if grep -q "_needle_build_time_filter" "$PROJECT_ROOT/src/cli/logs.sh" 2>/dev/null; then
    pass "logs.sh has _needle_build_time_filter function"
else
    fail "logs.sh missing _needle_build_time_filter function"
fi

echo ""

# ============================================================================
# Test: Log Filtering by Worker Name
# ============================================================================

echo "=== Worker Name Filtering Tests ==="

# Filter to only alpha logs
ALPHA_OUTPUT=$(_needle_logs alpha --raw 2>&1 || true)

if echo "$ALPHA_OUTPUT" | grep -q "needle-alpha"; then
    pass "_needle_logs alpha: shows alpha worker logs"
else
    fail "_needle_logs alpha: missing alpha worker logs"
fi

if ! echo "$ALPHA_OUTPUT" | grep -q "needle-bravo"; then
    pass "_needle_logs alpha: excludes bravo worker logs"
else
    fail "_needle_logs alpha: should not show bravo worker logs"
fi

# Filter to only bravo logs
BRAVO_OUTPUT=$(_needle_logs bravo --raw 2>&1 || true)

if echo "$BRAVO_OUTPUT" | grep -q "needle-bravo"; then
    pass "_needle_logs bravo: shows bravo worker logs"
else
    fail "_needle_logs bravo: missing bravo worker logs"
fi

if ! echo "$BRAVO_OUTPUT" | grep -q "needle-alpha"; then
    pass "_needle_logs bravo: excludes alpha worker logs"
else
    fail "_needle_logs bravo: should not show alpha worker logs"
fi

# Non-existent worker: NEEDLE_QUIET suppresses _needle_warn/_needle_info to stderr,
# but _needle_print (available workers list) still goes to stdout.
NOWORKER_OUTPUT=$(_needle_logs nonexistent-worker 2>&1 || true)
if echo "$NOWORKER_OUTPUT" | grep -q "needle-alpha\|needle-bravo"; then
    pass "_needle_logs nonexistent-worker: lists available workers"
else
    fail "_needle_logs nonexistent-worker: should list available workers (got: $NOWORKER_OUTPUT)"
fi

echo ""

# ============================================================================
# Test: Log Filtering by Time Range
# ============================================================================

echo "=== Time Range Filtering Tests ==="

# Test _needle_parse_time_arg with ISO date
TS_ISO=$(_needle_parse_time_arg "2024-01-15" 2>/dev/null || true)
if [[ "$TS_ISO" == *"2024-01-15"* ]]; then
    pass "_needle_parse_time_arg: parses ISO date format"
else
    fail "_needle_parse_time_arg: failed to parse ISO date (got: $TS_ISO)"
fi

# Test with full ISO timestamp
TS_FULL=$(_needle_parse_time_arg "2024-01-15T10:00:00Z" 2>/dev/null || true)
if [[ "$TS_FULL" == "2024-01-15T10:00:00Z" ]]; then
    pass "_needle_parse_time_arg: returns ISO timestamp unchanged"
else
    fail "_needle_parse_time_arg: failed to preserve ISO timestamp (got: $TS_FULL)"
fi

# Test relative time format
TS_1H=$(_needle_parse_time_arg "1h" 2>/dev/null || true)
if [[ -n "$TS_1H" ]] && [[ "$TS_1H" =~ [0-9]{4}-[0-9]{2}-[0-9]{2}T ]]; then
    pass "_needle_parse_time_arg: parses relative 1h format"
else
    fail "_needle_parse_time_arg: failed to parse relative 1h format (got: $TS_1H)"
fi

TS_30M=$(_needle_parse_time_arg "30m" 2>/dev/null || true)
if [[ -n "$TS_30M" ]] && [[ "$TS_30M" =~ [0-9]{4}-[0-9]{2}-[0-9]{2}T ]]; then
    pass "_needle_parse_time_arg: parses relative 30m format"
else
    fail "_needle_parse_time_arg: failed to parse relative 30m format (got: $TS_30M)"
fi

TS_2D=$(_needle_parse_time_arg "2d" 2>/dev/null || true)
if [[ -n "$TS_2D" ]] && [[ "$TS_2D" =~ [0-9]{4}-[0-9]{2}-[0-9]{2}T ]]; then
    pass "_needle_parse_time_arg: parses relative 2d format"
else
    fail "_needle_parse_time_arg: failed to parse relative 2d format (got: $TS_2D)"
fi

# Test _needle_build_time_filter function
TIME_FILTER=$(_needle_build_time_filter "2024-01-15T09:30:00Z" "" 2>/dev/null || true)
if echo "$TIME_FILTER" | grep -q "select(.ts >= "; then
    pass "_needle_build_time_filter: generates since filter"
else
    fail "_needle_build_time_filter: missing since filter (got: $TIME_FILTER)"
fi

TIME_FILTER_UNTIL=$(_needle_build_time_filter "" "2024-01-15T11:00:00Z" 2>/dev/null || true)
if echo "$TIME_FILTER_UNTIL" | grep -q "select(.ts <= "; then
    pass "_needle_build_time_filter: generates until filter"
else
    fail "_needle_build_time_filter: missing until filter (got: $TIME_FILTER_UNTIL)"
fi

TIME_FILTER_BOTH=$(_needle_build_time_filter "2024-01-15T09:30:00Z" "2024-01-15T11:00:00Z" 2>/dev/null || true)
if echo "$TIME_FILTER_BOTH" | grep -q "select(.ts >= " && echo "$TIME_FILTER_BOTH" | grep -q "select(.ts <= "; then
    pass "_needle_build_time_filter: generates combined since+until filter"
else
    fail "_needle_build_time_filter: missing combined filter (got: $TIME_FILTER_BOTH)"
fi

# Test --since filtering on actual logs (requires jq)
if command -v jq >/dev/null 2>&1; then
    # Since after bravo worker started but before alpha entries - should show only some alpha entries
    SINCE_OUTPUT=$(_needle_logs alpha --raw --since "2024-01-15T10:01:00Z" 2>&1 || true)
    if echo "$SINCE_OUTPUT" | grep -q "bead.claimed"; then
        pass "_needle_logs --since: includes entries after since timestamp"
    else
        fail "_needle_logs --since: missing expected entries after since timestamp"
    fi

    if ! echo "$SINCE_OUTPUT" | grep -q "worker.started"; then
        pass "_needle_logs --since: excludes entries before since timestamp"
    else
        fail "_needle_logs --since: should not include entries before since timestamp"
    fi

    # Until filter
    UNTIL_OUTPUT=$(_needle_logs alpha --raw --until "2024-01-15T10:01:30Z" 2>&1 || true)
    if echo "$UNTIL_OUTPUT" | grep -q "worker.started"; then
        pass "_needle_logs --until: includes entries before until timestamp"
    else
        fail "_needle_logs --until: missing expected entries before until timestamp"
    fi

    if ! echo "$UNTIL_OUTPUT" | grep -q "bead.completed"; then
        pass "_needle_logs --until: excludes entries after until timestamp"
    else
        fail "_needle_logs --until: should not include entries after until timestamp"
    fi
fi

echo ""

# ============================================================================
# Test: --follow Flag Behavior
# ============================================================================

echo "=== Follow Flag Tests ==="

# Test --follow flag recognition using a non-existent worker, which causes
# _needle_logs to exit early (before calling tail -f) with a "no files" message.
# This verifies the flag is parsed correctly without hanging.
FOLLOW_NONEXISTENT=$(
    export NEEDLE_HOME="$TEST_NEEDLE_HOME"
    _needle_logs --follow nonexistent-worker-zz 2>&1 || true
)

if ! echo "$FOLLOW_NONEXISTENT" | grep -qi "unknown option.*follow\|unrecognized.*follow"; then
    pass "_needle_logs: --follow flag is recognized (no unknown-option error)"
else
    fail "_needle_logs: --follow flag incorrectly rejected as unknown option"
fi

# Test -f short option likewise
FOLLOW_F_NONEXISTENT=$(
    export NEEDLE_HOME="$TEST_NEEDLE_HOME"
    _needle_logs -f nonexistent-worker-zz 2>&1 || true
)

if ! echo "$FOLLOW_F_NONEXISTENT" | grep -qi "unknown option.*-f\b\|unrecognized.*-f"; then
    pass "_needle_logs: -f short flag is recognized"
else
    fail "_needle_logs: -f short flag incorrectly rejected"
fi

# Verify logs.sh handles --follow in the source code
if grep -q "\-f|\-\-follow" "$PROJECT_ROOT/src/cli/logs.sh" 2>/dev/null; then
    pass "_needle_logs: --follow/-f is handled in source code"
else
    fail "_needle_logs: --follow/-f missing from source code"
fi

# Verify --follow triggers tail -f behavior in source code
if grep -q "tail -f" "$PROJECT_ROOT/src/cli/logs.sh" 2>/dev/null; then
    pass "_needle_logs: --follow uses tail -f for streaming"
else
    fail "_needle_logs: --follow missing tail -f implementation"
fi

echo ""

# ============================================================================
# Test: --json Flag for Structured Output
# ============================================================================

echo "=== JSON Output Tests ==="

if command -v jq >/dev/null 2>&1; then
    # Test --json output is valid JSON array
    JSON_OUTPUT=$(_needle_logs alpha --json 2>&1 || true)
    if echo "$JSON_OUTPUT" | jq -e 'type == "array"' >/dev/null 2>&1; then
        pass "_needle_logs --json: outputs valid JSON array"
    else
        fail "_needle_logs --json: invalid JSON array output (got: $JSON_OUTPUT)"
    fi

    # Test JSON array contains log entries
    JSON_COUNT=$(echo "$JSON_OUTPUT" | jq 'length' 2>/dev/null || echo "0")
    if [[ "$JSON_COUNT" -gt 0 ]]; then
        pass "_needle_logs --json: JSON array has entries"
    else
        fail "_needle_logs --json: JSON array is empty (count: $JSON_COUNT)"
    fi

    # Test -j short option
    JSON_SHORT_OUTPUT=$(_needle_logs alpha -j 2>&1 || true)
    if echo "$JSON_SHORT_OUTPUT" | jq -e 'type == "array"' >/dev/null 2>&1; then
        pass "_needle_logs -j: short option outputs valid JSON array"
    else
        fail "_needle_logs -j: short option invalid JSON output"
    fi

    # Test --json combined with event filter
    JSON_FILTERED=$(_needle_logs alpha --json --event bead.completed 2>&1 || true)
    if echo "$JSON_FILTERED" | jq -e 'type == "array"' >/dev/null 2>&1; then
        pass "_needle_logs --json --event: outputs valid JSON array with filter"
    else
        fail "_needle_logs --json --event: invalid filtered JSON output"
    fi

    # Verify filtered JSON only has matching events
    BAD_EVENTS=$(echo "$JSON_FILTERED" | jq '[.[] | select(.event != "bead.completed")] | length' 2>/dev/null || echo "0")
    if [[ "$BAD_EVENTS" -eq 0 ]]; then
        pass "_needle_logs --json --event: filtered output only contains matching events"
    else
        fail "_needle_logs --json --event: filtered output has non-matching events"
    fi

    # Test --json with all workers (no worker specified)
    JSON_ALL=$(_needle_logs --json 2>&1 || true)
    if echo "$JSON_ALL" | jq -e 'type == "array"' >/dev/null 2>&1; then
        pass "_needle_logs --json (all workers): outputs valid JSON array"
    else
        fail "_needle_logs --json (all workers): invalid JSON output"
    fi

    # All workers should have more entries than single worker
    JSON_ALL_COUNT=$(echo "$JSON_ALL" | jq 'length' 2>/dev/null || echo "0")
    if [[ "$JSON_ALL_COUNT" -gt "$JSON_COUNT" ]]; then
        pass "_needle_logs --json (all workers): contains more entries than single worker"
    else
        fail "_needle_logs --json (all workers): should have more entries than single worker (all: $JSON_ALL_COUNT, alpha: $JSON_COUNT)"
    fi
else
    pass "_needle_logs --json: skipped (jq not available)"
fi

echo ""

# ============================================================================
# Test: --raw Flag
# ============================================================================

echo "=== Raw Output Tests ==="

RAW_OUTPUT=$(_needle_logs alpha --raw 2>&1 || true)

# Raw output should contain JSONL entries
if echo "$RAW_OUTPUT" | grep -q '"event"'; then
    pass "_needle_logs --raw: output contains JSONL fields"
else
    fail "_needle_logs --raw: missing JSONL fields in output"
fi

# Raw output should not be formatted (no extra spacing)
if echo "$RAW_OUTPUT" | grep -q '"ts"'; then
    pass "_needle_logs --raw: output preserves JSON structure"
else
    fail "_needle_logs --raw: missing ts field in raw output"
fi

echo ""

# ============================================================================
# Test: --lines Option
# ============================================================================

echo "=== Lines Limiting Tests ==="

# Default is 50 lines, but our test file has 5 lines so all should show
ALL_OUTPUT=$(_needle_logs alpha --raw 2>&1 || true)
LINE_COUNT=$(echo "$ALL_OUTPUT" | wc -l | tr -d ' ')

# Test --lines=2 limits output to 2 lines
TWO_LINES_OUTPUT=$(_needle_logs alpha --raw --lines 2 2>&1 || true)
TWO_LINE_COUNT=$(echo "$TWO_LINES_OUTPUT" | grep -c '"event"' || true)

if [[ "$TWO_LINE_COUNT" -le 2 ]]; then
    pass "_needle_logs --lines 2: limits output to 2 entries"
else
    fail "_needle_logs --lines 2: should limit output to 2 (got: $TWO_LINE_COUNT)"
fi

# Test --lines= format
TWO_LINES_EQ=$(_needle_logs alpha --raw --lines=1 2>&1 || true)
ONE_LINE_COUNT=$(echo "$TWO_LINES_EQ" | grep -c '"event"' || true)

if [[ "$ONE_LINE_COUNT" -le 1 ]]; then
    pass "_needle_logs --lines=1: limits output to 1 entry"
else
    fail "_needle_logs --lines=1: should limit output to 1 (got: $ONE_LINE_COUNT)"
fi

# Test -n short option
N_OUTPUT=$(_needle_logs alpha --raw -n 3 2>&1 || true)
N_COUNT=$(echo "$N_OUTPUT" | grep -c '"event"' || true)

if [[ "$N_COUNT" -le 3 ]]; then
    pass "_needle_logs -n 3: limits output to 3 entries"
else
    fail "_needle_logs -n 3: should limit output to 3 (got: $N_COUNT)"
fi

echo ""

# ============================================================================
# Test: Event Filtering
# ============================================================================

echo "=== Event Filtering Tests ==="

if command -v jq >/dev/null 2>&1; then
    # Filter by exact event type
    EVENT_OUTPUT=$(_needle_logs alpha --raw --event bead.completed 2>&1 || true)
    if echo "$EVENT_OUTPUT" | grep -q '"bead.completed"'; then
        pass "_needle_logs --event bead.completed: includes matching events"
    else
        fail "_needle_logs --event bead.completed: missing matching events"
    fi

    if ! echo "$EVENT_OUTPUT" | grep -q '"worker.started"'; then
        pass "_needle_logs --event bead.completed: excludes non-matching events"
    else
        fail "_needle_logs --event bead.completed: should not include worker.started"
    fi

    # Filter by wildcard event type (bead.*)
    BEAD_OUTPUT=$(_needle_logs alpha --raw --event 'bead.*' 2>&1 || true)
    if echo "$BEAD_OUTPUT" | grep -q '"bead\.'; then
        pass "_needle_logs --event 'bead.*': includes bead events"
    else
        fail "_needle_logs --event 'bead.*': missing bead events"
    fi

    if ! echo "$BEAD_OUTPUT" | grep -q '"worker.started"'; then
        pass "_needle_logs --event 'bead.*': excludes non-bead events"
    else
        fail "_needle_logs --event 'bead.*': should not include worker.started"
    fi
fi

echo ""

# ============================================================================
# Test: Bead ID Filtering
# ============================================================================

echo "=== Bead Filtering Tests ==="

if command -v jq >/dev/null 2>&1; then
    BEAD_FILTER_OUTPUT=$(_needle_logs alpha --raw --bead nd-abc1 2>&1 || true)
    if echo "$BEAD_FILTER_OUTPUT" | grep -q '"nd-abc1"'; then
        pass "_needle_logs --bead nd-abc1: includes matching bead entries"
    else
        fail "_needle_logs --bead nd-abc1: missing entries for bead nd-abc1"
    fi

    if ! echo "$BEAD_FILTER_OUTPUT" | grep -q '"nd-abc2"'; then
        pass "_needle_logs --bead nd-abc1: excludes other bead entries"
    else
        fail "_needle_logs --bead nd-abc1: should not include nd-abc2"
    fi
fi

echo ""

# ============================================================================
# Test: Error Cases
# ============================================================================

echo "=== Error Cases Tests ==="

# Non-existent log directory: use NEEDLE_QUIET=false to get warning output on stderr
NO_LOG_DIR=$(NEEDLE_QUIET=false NEEDLE_HOME="$TEST_DIR/no-such-dir" _needle_logs 2>&1 || true)
if echo "$NO_LOG_DIR" | grep -qi "no logs\|not found\|workers will"; then
    pass "_needle_logs: handles missing log directory gracefully"
else
    fail "_needle_logs: should warn about missing log directory (got: $NO_LOG_DIR)"
fi

# Empty log directory: same approach
EMPTY_LOG_DIR="$TEST_DIR/empty-logs-needle/logs"
mkdir -p "$EMPTY_LOG_DIR"
EMPTY_LOG_OUTPUT=$(NEEDLE_QUIET=false NEEDLE_HOME="$TEST_DIR/empty-logs-needle" _needle_logs 2>&1 || true)
if echo "$EMPTY_LOG_OUTPUT" | grep -qi "no log files\|workers will"; then
    pass "_needle_logs: handles empty log directory gracefully"
else
    fail "_needle_logs: should warn about empty log directory (got: $EMPTY_LOG_OUTPUT)"
fi

# Invalid option: use a plain subshell (no command substitution) to capture exit code
# without mixing stdout into the exit-code variable
( _needle_logs --invalid-unknown-option >/dev/null 2>/dev/null )
INVALID_OPT_EXIT=$?
if [[ "$INVALID_OPT_EXIT" -ne 0 ]]; then
    pass "_needle_logs: rejects unknown options with non-zero exit"
else
    fail "_needle_logs: should reject unknown options"
fi

INVALID_OPT_MSG=$(_needle_logs --invalid-unknown-option 2>&1 || true)
if echo "$INVALID_OPT_MSG" | grep -qi "unknown option\|invalid"; then
    pass "_needle_logs: shows error message for unknown options"
else
    fail "_needle_logs: missing error message for unknown options (got: $INVALID_OPT_MSG)"
fi

echo ""

# ============================================================================
# Test: No Log Files for Specific Worker
# ============================================================================

echo "=== Missing Worker Logs Tests ==="

# Non-existent worker with existing log dir: warning to stderr is suppressed by NEEDLE_QUIET,
# but the available workers list is still printed to stdout via _needle_print.
NO_WORKER_OUTPUT=$(_needle_logs zulu-worker 2>&1 || true)
if echo "$NO_WORKER_OUTPUT" | grep -q "needle-alpha\|needle-bravo"; then
    pass "_needle_logs zulu-worker: lists available workers when worker not found"
else
    fail "_needle_logs zulu-worker: should list available workers (got: $NO_WORKER_OUTPUT)"
fi

# Non-existent worker should list available workers
if echo "$NO_WORKER_OUTPUT" | grep -qi "available\|alpha\|bravo"; then
    pass "_needle_logs zulu-worker: lists available workers"
else
    fail "_needle_logs zulu-worker: should list available workers (got: $NO_WORKER_OUTPUT)"
fi

echo ""

# ============================================================================
# Test: Format Log Line
# ============================================================================

echo "=== Format Log Line Tests ==="

if command -v jq >/dev/null 2>&1; then
    FORMAT_OUTPUT=$(_needle_format_log_line '{"ts":"2024-01-15T10:01:00Z","event":"bead.completed","session":"needle-alpha","data":{"bead_id":"nd-abc1"}}' 2>&1 || true)

    if echo "$FORMAT_OUTPUT" | grep -q "10:01:00"; then
        pass "_needle_format_log_line: extracts time from timestamp"
    else
        fail "_needle_format_log_line: missing time in output (got: $FORMAT_OUTPUT)"
    fi

    if echo "$FORMAT_OUTPUT" | grep -q "bead.completed"; then
        pass "_needle_format_log_line: includes event type"
    else
        fail "_needle_format_log_line: missing event type (got: $FORMAT_OUTPUT)"
    fi
fi

echo ""

# ============================================================================
# Summary
# ============================================================================

echo "=== Summary ==="
echo "Passed: $TESTS_PASSED"
echo "Failed: $TESTS_FAILED"

if [[ $TESTS_FAILED -eq 0 ]]; then
    printf "${GREEN}All tests passed!${NC}\n"
    exit 0
else
    printf "${RED}Some tests failed!${NC}\n"
    exit 1
fi
