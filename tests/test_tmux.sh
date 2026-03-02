#!/usr/bin/env bash
# Tests for NEEDLE tmux session management (src/runner/tmux.sh)

# Test setup - create temp directory
TEST_DIR=$(mktemp -d)
TEST_NEEDLE_HOME="$TEST_DIR/.needle"

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_HOME="$TEST_NEEDLE_HOME"
export NEEDLE_STATE_DIR="state"
export NEEDLE_WORKERS_FILE="$TEST_NEEDLE_HOME/state/workers.json"
export NEEDLE_CONFIG_FILE="$TEST_NEEDLE_HOME/config.yaml"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=true

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/runner/tmux.sh"

# Cleanup function
cleanup() {
    # Kill any test sessions we created
    for session in $(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep '^test-needle-' || true); do
        tmux kill-session -t "$session" 2>/dev/null || true
    done
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

# Test counter
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

# Test helper
test_case() {
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

# Check if tmux is available for tests that need it
tmux_available() {
    command -v tmux &>/dev/null
}

# ============ Tests ============

# Test: _needle_in_tmux returns false outside tmux
test_case "_needle_in_tmux returns false when not in tmux"
# Clear TMUX variable for test
OLD_TMUX="${TMUX:-}"
unset TMUX
if ! _needle_in_tmux; then
    test_pass
else
    test_fail "Should return false outside tmux"
fi
# Restore TMUX
[[ -n "$OLD_TMUX" ]] && export TMUX="$OLD_TMUX"

# Test: _needle_in_tmux returns true inside tmux
test_case "_needle_in_tmux returns true when TMUX is set"
export TMUX="/tmp/tmux-1000/default,1234,0"
if _needle_in_tmux; then
    test_pass
else
    test_fail "Should return true when TMUX is set"
fi
unset TMUX

# Test: _needle_tmux_available
test_case "_needle_tmux_available returns correct result"
if tmux_available; then
    if _needle_tmux_available; then
        test_pass
    else
        test_fail "tmux is installed but function returned false"
    fi
else
    if ! _needle_tmux_available; then
        test_pass
    else
        test_fail "tmux is not installed but function returned true"
    fi
fi

# Test: _needle_generate_session_name with default pattern
test_case "_needle_generate_session_name with default pattern"
name=$(_needle_generate_session_name "" "claude" "anthropic" "sonnet" "alpha")
if [[ "$name" == "needle-claude-anthropic-sonnet-alpha" ]]; then
    test_pass
else
    test_fail "Expected 'needle-claude-anthropic-sonnet-alpha', got '$name'"
fi

# Test: _needle_generate_session_name with custom pattern
test_case "_needle_generate_session_name with custom pattern"
name=$(_needle_generate_session_name "worker-{provider}-{model}-{identifier}" "claude" "anthropic" "opus" "bravo")
if [[ "$name" == "worker-anthropic-opus-bravo" ]]; then
    test_pass
else
    test_fail "Expected 'worker-anthropic-opus-bravo', got '$name'"
fi

# Test: _needle_generate_session_name sanitizes special characters
test_case "_needle_generate_session_name sanitizes special characters"
name=$(_needle_generate_session_name "" "my runner" "my@provider" "my#model" "test")
if [[ "$name" != *" "* && "$name" != *"@"* && "$name" != *"#"* ]]; then
    test_pass
else
    test_fail "Name should not contain special chars: '$name'"
fi

# Test: _needle_parse_session_name extracts components
test_case "_needle_parse_session_name extracts components correctly"
_needle_parse_session_name "needle-claude-anthropic-sonnet-alpha"
if [[ "$NEEDLE_SESSION_RUNNER" == "claude" && \
      "$NEEDLE_SESSION_PROVIDER" == "anthropic" && \
      "$NEEDLE_SESSION_MODEL" == "sonnet" && \
      "$NEEDLE_SESSION_IDENTIFIER" == "alpha" ]]; then
    test_pass
else
    test_fail "Components: runner=$NEEDLE_SESSION_RUNNER, provider=$NEEDLE_SESSION_PROVIDER, model=$NEEDLE_SESSION_MODEL, id=$NEEDLE_SESSION_IDENTIFIER"
fi

# Test: _needle_parse_session_name fails with empty input
test_case "_needle_parse_session_name fails with empty input"
if ! _needle_parse_session_name ""; then
    test_pass
else
    test_fail "Should fail with empty input"
fi

# Test: _needle_parse_session_name fails with invalid format
test_case "_needle_parse_session_name fails with invalid format"
if ! _needle_parse_session_name "invalid-format"; then
    test_pass
else
    test_fail "Should fail with invalid format"
fi

# Test: _needle_parse_session_name uses default identifier
test_case "_needle_parse_session_name uses alpha as default identifier"
_needle_parse_session_name "needle-claude-anthropic-sonnet"
if [[ "$NEEDLE_SESSION_IDENTIFIER" == "alpha" ]]; then
    test_pass
else
    test_fail "Expected 'alpha', got '$NEEDLE_SESSION_IDENTIFIER'"
fi

# ============ Tests requiring tmux ============

if tmux_available; then
    # Test: _needle_session_exists returns false for non-existent session
    test_case "_needle_session_exists returns false for non-existent session"
    if ! _needle_session_exists "test-needle-nonexistent-12345"; then
        test_pass
    else
        test_fail "Should return false for non-existent session"
    fi

    # Test: _needle_create_session creates a session
    test_case "_needle_create_session creates a detached session"
    if _needle_create_session "test-needle-create-1" "sleep 60"; then
        if _needle_session_exists "test-needle-create-1"; then
            tmux kill-session -t "test-needle-create-1" 2>/dev/null || true
            test_pass
        else
            test_fail "Session was not created"
        fi
    else
        test_fail "create_session returned failure"
    fi

    # Test: _needle_create_session fails for existing session
    test_case "_needle_create_session fails for existing session"
    tmux new-session -d -s "test-needle-create-2" "sleep 10" 2>/dev/null || true
    if ! _needle_create_session "test-needle-create-2" "echo test"; then
        tmux kill-session -t "test-needle-create-2" 2>/dev/null || true
        test_pass
    else
        tmux kill-session -t "test-needle-create-2" 2>/dev/null || true
        test_fail "Should fail for existing session"
    fi

    # Test: _needle_kill_session terminates a session
    test_case "_needle_kill_session terminates a session"
    tmux new-session -d -s "test-needle-kill-1" "sleep 10" 2>/dev/null || true
    if _needle_kill_session "test-needle-kill-1"; then
        if ! _needle_session_exists "test-needle-kill-1"; then
            test_pass
        else
            test_fail "Session still exists after kill"
        fi
    else
        tmux kill-session -t "test-needle-kill-1" 2>/dev/null || true
        test_fail "kill_session returned failure"
    fi

    # Test: _needle_list_sessions filters to needle- prefix
    test_case "_needle_list_sessions filters to needle- prefix"
    # Create some test sessions (must start with needle- to match filter)
    tmux new-session -d -s "needle-test-list-1" "sleep 10" 2>/dev/null || true
    tmux new-session -d -s "needle-test-list-2" "sleep 10" 2>/dev/null || true

    sessions=$(_needle_list_sessions)
    if [[ "$sessions" == *"needle-test-list-1"* && "$sessions" == *"needle-test-list-2"* ]]; then
        test_pass
    else
        test_fail "Expected sessions to contain needle-test-list-1 and needle-test-list-2, got: $sessions"
    fi

    # Cleanup
    tmux kill-session -t "needle-test-list-1" 2>/dev/null || true
    tmux kill-session -t "needle-test-list-2" 2>/dev/null || true

    # Test: _needle_count_sessions returns correct count
    test_case "_needle_count_sessions returns correct count"
    # Create sessions (must start with needle- to match filter)
    tmux new-session -d -s "needle-test-count-1" "sleep 10" 2>/dev/null || true
    tmux new-session -d -s "needle-test-count-2" "sleep 10" 2>/dev/null || true
    tmux new-session -d -s "needle-test-count-3" "sleep 10" 2>/dev/null || true

    count=$(_needle_count_sessions)
    if [[ "$count" -ge 3 ]]; then
        test_pass
    else
        test_fail "Expected at least 3 sessions, got $count"
    fi

    # Cleanup
    tmux kill-session -t "needle-test-count-1" 2>/dev/null || true
    tmux kill-session -t "needle-test-count-2" 2>/dev/null || true
    tmux kill-session -t "needle-test-count-3" 2>/dev/null || true

    # Test: _needle_send_to_session sends command
    test_case "_needle_send_to_session sends command to session"
    tmux new-session -d -s "test-needle-send-1" "sleep 10" 2>/dev/null || true
    if _needle_send_to_session "test-needle-send-1" "echo 'hello'"; then
        tmux kill-session -t "test-needle-send-1" 2>/dev/null || true
        test_pass
    else
        tmux kill-session -t "test-needle-send-1" 2>/dev/null || true
        test_fail "send_to_session returned failure"
    fi

    # Test: _needle_list_sessions_json returns valid JSON
    test_case "_needle_list_sessions_json returns valid JSON"
    tmux new-session -d -s "needle-test-json-1" "sleep 10" 2>/dev/null || true

    json=$(_needle_list_sessions_json)
    if echo "$json" | jq -e '.[] | .session' &>/dev/null; then
        tmux kill-session -t "needle-test-json-1" 2>/dev/null || true
        test_pass
    else
        tmux kill-session -t "needle-test-json-1" 2>/dev/null || true
        test_fail "Invalid JSON or missing .session: $json"
    fi

    # Test: _needle_get_session_info returns valid JSON
    test_case "_needle_get_session_info returns valid JSON"
    tmux new-session -d -s "test-needle-info-1" "sleep 10" 2>/dev/null || true

    json=$(_needle_get_session_info "test-needle-info-1")
    if echo "$json" | jq -e '.session' &>/dev/null; then
        tmux kill-session -t "test-needle-info-1" 2>/dev/null || true
        test_pass
    else
        tmux kill-session -t "test-needle-info-1" 2>/dev/null || true
        test_fail "Invalid JSON: $json"
    fi

    # Test: _needle_next_identifier returns first unused NATO name
    test_case "_needle_next_identifier returns alpha for no existing sessions"
    identifier=$(_needle_next_identifier "testrunner" "testprovider" "testmodel")
    if [[ "$identifier" == "alpha" ]]; then
        test_pass
    else
        test_fail "Expected 'alpha', got '$identifier'"
    fi

    # Test: _needle_next_identifier skips used NATO names
    test_case "_needle_next_identifier skips used NATO names"
    tmux new-session -d -s "needle-testrunner-testprovider-testmodel-alpha" "sleep 10" 2>/dev/null || true
    identifier=$(_needle_next_identifier "testrunner" "testprovider" "testmodel")
    if [[ "$identifier" == "bravo" ]]; then
        test_pass
    else
        test_fail "Expected 'bravo', got '$identifier'"
    fi
    tmux kill-session -t "needle-testrunner-testprovider-testmodel-alpha" 2>/dev/null || true

    # Test: _needle_count_agent_sessions counts correctly
    test_case "_needle_count_agent_sessions counts agent sessions"
    tmux new-session -d -s "needle-claude-anthropic-sonnet-test1" "sleep 10" 2>/dev/null || true
    tmux new-session -d -s "needle-claude-anthropic-sonnet-test2" "sleep 10" 2>/dev/null || true

    count=$(_needle_count_agent_sessions "claude-anthropic-sonnet")
    if [[ "$count" -ge 2 ]]; then
        test_pass
    else
        test_fail "Expected at least 2 sessions, got $count"
    fi

    # Cleanup
    tmux kill-session -t "needle-claude-anthropic-sonnet-test1" 2>/dev/null || true
    tmux kill-session -t "needle-claude-anthropic-sonnet-test2" 2>/dev/null || true

    # Test: _needle_is_session_attached returns correct status
    test_case "_needle_is_session_attached returns false for detached session"
    tmux new-session -d -s "test-needle-attach-1" "sleep 10" 2>/dev/null || true
    if ! _needle_is_session_attached "test-needle-attach-1"; then
        test_pass
    else
        test_fail "New session should not be attached"
    fi
    tmux kill-session -t "test-needle-attach-1" 2>/dev/null || true

    # Test: _needle_rename_session works
    test_case "_needle_rename_session renames session"
    tmux new-session -d -s "test-needle-rename-old" "sleep 10" 2>/dev/null || true
    if _needle_rename_session "test-needle-rename-old" "test-needle-rename-new"; then
        if _needle_session_exists "test-needle-rename-new" && ! _needle_session_exists "test-needle-rename-old"; then
            tmux kill-session -t "test-needle-rename-new" 2>/dev/null || true
            test_pass
        else
            tmux kill-session -t "test-needle-rename-old" 2>/dev/null || true
            tmux kill-session -t "test-needle-rename-new" 2>/dev/null || true
            test_fail "Session rename didn't work correctly"
        fi
    else
        tmux kill-session -t "test-needle-rename-old" 2>/dev/null || true
        test_fail "rename_session returned failure"
    fi

else
    echo ""
    echo "================================"
    echo "Skipping tmux-dependent tests (tmux not available)"
    echo "================================"
fi

# ============ Summary ============
echo ""
echo "================================"
echo "Test Summary"
echo "================================"
echo "Tests run:    $TESTS_RUN"
echo "Tests passed: $TESTS_PASSED"
echo "Tests failed: $TESTS_FAILED"
echo "================================"

if [[ $TESTS_FAILED -gt 0 ]]; then
    exit 1
fi

exit 0
