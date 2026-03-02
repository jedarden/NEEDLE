#!/usr/bin/env bash
# Test script for telemetry/writer.sh

set -e

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_DIR="$(dirname "$SCRIPT_DIR")/src"

# Source required modules
source "$SRC_DIR/lib/constants.sh"
source "$SRC_DIR/lib/output.sh"
source "$SRC_DIR/lib/json.sh"
source "$SRC_DIR/lib/utils.sh"
source "$SRC_DIR/telemetry/writer.sh"

# Test utilities
TESTS_PASSED=0
TESTS_FAILED=0

_test_start() {
    echo -n "Testing: $1... "
}

_test_pass() {
    echo "✓ PASS"
    TESTS_PASSED=$((TESTS_PASSED + 1))
}

_test_fail() {
    echo "✗ FAIL: $1"
    TESTS_FAILED=$((TESTS_FAILED + 1))
}

# Setup test environment
setup() {
    NEEDLE_HOME=$(mktemp -d)
    NEEDLE_SESSION="test-session-$$"
    NEEDLE_VERBOSE="false"
    _needle_output_init
}

# Cleanup test environment
cleanup() {
    if [[ -n "$NEEDLE_HOME" ]] && [[ -d "$NEEDLE_HOME" ]]; then
        rm -rf "$NEEDLE_HOME"
    fi
}

# Test 1: Log initialization
test_init_log() {
    _test_start "init_log creates log file"

    setup
    _needle_init_log "test-session"

    if [[ -f "$NEEDLE_LOG_FILE" ]] && [[ "$NEEDLE_LOG_INITIALIZED" == "true" ]]; then
        _test_pass
    else
        _test_fail "Log file not created or not initialized"
    fi

    cleanup
}

# Test 2: Log initialization creates directory
test_init_log_creates_dir() {
    _test_start "init_log creates log directory if missing"

    setup
    # Remove logs directory
    rm -rf "$NEEDLE_HOME/$NEEDLE_LOG_DIR"

    _needle_init_log "test-session"

    if [[ -d "$NEEDLE_HOME/$NEEDLE_LOG_DIR" ]] && [[ -f "$NEEDLE_LOG_FILE" ]]; then
        _test_pass
    else
        _test_fail "Log directory not created"
    fi

    cleanup
}

# Test 3: Write event
test_write_event() {
    _test_start "write_event writes JSON to log"

    setup
    _needle_init_log "test-session"

    _needle_write_event '{"type":"test","message":"hello"}'

    if grep -q '"type":"test"' "$NEEDLE_LOG_FILE" && grep -q '"message":"hello"' "$NEEDLE_LOG_FILE"; then
        _test_pass
    else
        _test_fail "Event not written to log"
    fi

    cleanup
}

# Test 4: Write event with verbose mode
test_write_event_verbose() {
    _test_start "write_event mirrors to stdout in verbose mode"

    setup
    _needle_init_log "test-session"
    NEEDLE_VERBOSE="true"

    local output
    output=$(_needle_write_event '{"type":"test","verbose":true}')

    if [[ "$output" == *'"type":"test"'* ]]; then
        _test_pass
    else
        _test_fail "Event not mirrored to stdout in verbose mode"
    fi

    cleanup
}

# Test 5: Write formatted event
test_write_formatted_event() {
    _test_start "write_formatted_event creates proper JSON"

    setup
    _needle_init_log "test-session"

    _needle_write_formatted_event --type status --message "Running" --progress 50

    if grep -q '"type":"status"' "$NEEDLE_LOG_FILE" && \
       grep -q '"message":"Running"' "$NEEDLE_LOG_FILE" && \
       grep -q '"progress":50' "$NEEDLE_LOG_FILE"; then
        _test_pass
    else
        _test_fail "Formatted event not written correctly"
    fi

    cleanup
}

# Test 6: Multiple events appended correctly
test_multiple_events() {
    _test_start "multiple events are appended correctly"

    setup
    _needle_init_log "test-session"

    _needle_write_event '{"id":1}'
    _needle_write_event '{"id":2}'
    _needle_write_event '{"id":3}'

    local count
    count=$(wc -l < "$NEEDLE_LOG_FILE")

    if [[ "$count" -eq 3 ]]; then
        _test_pass
    else
        _test_fail "Expected 3 lines, got $count"
    fi

    cleanup
}

# Test 7: Log rotation
test_log_rotation() {
    _test_start "rotate_logs rotates when size exceeded"

    setup
    _needle_init_log "test-session"

    # Write some data
    echo '{"test":"data"}' >> "$NEEDLE_LOG_FILE"

    # Rotate with very small max size (1 byte)
    _needle_rotate_logs 1

    # Check that old file was renamed
    if ls "${NEEDLE_LOG_FILE}."*.old 2>/dev/null | head -1 | grep -q ".old"; then
        _test_pass
    else
        _test_fail "Log file was not rotated"
    fi

    cleanup
}

# Test 8: Log is initialized check
test_log_is_initialized() {
    _test_start "log_is_initialized returns correct status"

    setup

    # Should be false before init
    if _needle_log_is_initialized; then
        _test_fail "log_is_initialized should be false before init"
        cleanup
        return
    fi

    _needle_init_log "test-session"

    # Should be true after init
    if _needle_log_is_initialized; then
        _test_pass
    else
        _test_fail "log_is_initialized should be true after init"
    fi

    cleanup
}

# Test 9: Log tail
test_log_tail() {
    _test_start "log_tail returns last N events"

    setup
    _needle_init_log "test-session"

    _needle_write_event '{"id":"first"}'
    _needle_write_event '{"id":"second"}'
    _needle_write_event '{"id":"third"}'

    local last_two
    last_two=$(_needle_log_tail 2)

    local count
    count=$(echo "$last_two" | wc -l)

    if [[ "$count" -eq 2 ]] && echo "$last_two" | grep -q '"id":"third"'; then
        _test_pass
    else
        _test_fail "log_tail did not return expected lines"
    fi

    cleanup
}

# Test 10: Telemetry convenience function
test_telemetry() {
    _test_start "telemetry convenience function works"

    setup
    _needle_init_log "test-session"

    _needle_telemetry "heartbeat" "Worker alive" "count=5"

    if grep -q '"type":"heartbeat"' "$NEEDLE_LOG_FILE" && \
       grep -q '"message":"Worker alive"' "$NEEDLE_LOG_FILE"; then
        _test_pass
    else
        _test_fail "Telemetry event not written correctly"
    fi

    cleanup
}

# Test 11: Empty event handling
test_empty_event() {
    _test_start "empty event is rejected"

    setup
    _needle_init_log "test-session"

    if ! _needle_write_event "" 2>/dev/null; then
        _test_pass
    else
        # Even if it "succeeds", the log should be empty
        if [[ ! -s "$NEEDLE_LOG_FILE" ]]; then
            _test_pass
        else
            _test_fail "Empty event should be rejected"
        fi
    fi

    cleanup
}

# Run all tests
main() {
    echo "=========================================="
    echo "Telemetry Writer Tests"
    echo "=========================================="
    echo

    test_init_log
    test_init_log_creates_dir
    test_write_event
    test_write_event_verbose
    test_write_formatted_event
    test_multiple_events
    test_log_rotation
    test_log_is_initialized
    test_log_tail
    test_telemetry
    test_empty_event

    echo
    echo "=========================================="
    echo "Results: $TESTS_PASSED passed, $TESTS_FAILED failed"
    echo "=========================================="

    if [[ "$TESTS_FAILED" -gt 0 ]]; then
        exit 1
    fi
}

main "$@"
