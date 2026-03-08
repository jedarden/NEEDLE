#!/usr/bin/env bash
# Tests for NEEDLE restart CLI (src/cli/restart.sh)

# Test setup
TEST_DIR=$(mktemp -d)
TEST_NEEDLE_HOME="$TEST_DIR/.needle"

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_HOME="$TEST_NEEDLE_HOME"
export NEEDLE_STATE_DIR="state"
export NEEDLE_SHUTDOWN_DIR="$TEST_NEEDLE_HOME/state/shutdown"
export NEEDLE_WORKERS_FILE="$TEST_NEEDLE_HOME/state/workers.json"
export NEEDLE_CONFIG_FILE="$TEST_NEEDLE_HOME/config.yaml"
export NEEDLE_QUIET=true

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/runner/tmux.sh"
source "$PROJECT_DIR/src/runner/state.sh"
source "$PROJECT_DIR/src/cli/stop.sh"
source "$PROJECT_DIR/src/cli/restart.sh"

# Cleanup function
cleanup() {
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

# Test counter
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

# Test helpers
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

# Set up state directory
setup_state_dir() {
    mkdir -p "$TEST_NEEDLE_HOME/state/shutdown"
    mkdir -p "$TEST_NEEDLE_HOME/state/heartbeats"
    echo '{"workers":[]}' > "$NEEDLE_WORKERS_FILE"
}

# ============ Tests ============

# ---- Help Tests ----

test_case "_needle_restart_help runs without error"
if _needle_restart_help >/dev/null 2>&1; then
    test_pass
else
    test_fail "Help function failed"
fi

test_case "_needle_restart_help contains expected options"
help_output=$(_needle_restart_help 2>&1)
if [[ "$help_output" == *"--all"* ]] && \
   [[ "$help_output" == *"--graceful"* ]] && \
   [[ "$help_output" == *"--immediate"* ]] && \
   [[ "$help_output" == *"--timeout"* ]]; then
    test_pass
else
    test_fail "Help missing expected options"
fi

test_case "_needle_restart_help contains usage examples"
help_output=$(_needle_restart_help 2>&1)
if [[ "$help_output" == *"needle restart"* ]] && \
   [[ "$help_output" == *"EXAMPLES"* ]]; then
    test_pass
else
    test_fail "Help missing usage examples"
fi

# ---- Argument Parsing Tests ----

test_case "_needle_restart: rejects no arguments"
(
    _needle_restart 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?
if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Should reject empty arguments"
fi

test_case "_needle_restart: rejects --all with specific workers"
(
    _needle_restart --all alpha 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?
if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Should reject --all with specific workers"
fi

test_case "_needle_restart: rejects unknown option"
(
    _needle_restart --unknown-flag 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?
if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Should reject unknown option"
fi

test_case "_needle_restart: --help exits successfully"
(
    _needle_restart --help 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?
if [[ $exit_code -eq 0 ]]; then
    test_pass
else
    test_fail "Help should exit successfully"
fi

test_case "_needle_restart: -h exits successfully"
(
    _needle_restart -h 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?
if [[ $exit_code -eq 0 ]]; then
    test_pass
else
    test_fail "Short help flag should exit successfully"
fi

test_case "_needle_restart: --timeout requires a value"
(
    _needle_restart --timeout 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?
if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Should reject --timeout without value"
fi

# ---- _needle_wait_for_idle Tests ----

test_case "_needle_wait_for_idle: returns 0 when session does not exist"
setup_state_dir

# Mock _needle_session_exists to return 1 (session doesn't exist)
_needle_session_exists() { return 1; }

if _needle_wait_for_idle "nonexistent-session" 10 2>/dev/null; then
    test_pass
else
    test_fail "Should return 0 when session doesn't exist"
fi

# Restore real _needle_session_exists
unset -f _needle_session_exists
source "$PROJECT_DIR/src/runner/tmux.sh" 2>/dev/null

test_case "_needle_wait_for_idle: returns 0 when no heartbeat file"
setup_state_dir

# Mock session as existing but no heartbeat file
_needle_session_exists() { return 0; }

heartbeat_dir="$TEST_NEEDLE_HOME/state/heartbeats"
rm -f "$heartbeat_dir/test-session.json"

if _needle_wait_for_idle "test-session" 5 2>/dev/null; then
    test_pass
else
    test_fail "Should return 0 when no heartbeat file"
fi

# Restore real _needle_session_exists
unset -f _needle_session_exists
source "$PROJECT_DIR/src/runner/tmux.sh" 2>/dev/null

test_case "_needle_wait_for_idle: returns 0 when current_bead is empty"
setup_state_dir

# Mock session as existing with heartbeat showing idle
_needle_session_exists() { return 0; }

heartbeat_dir="$TEST_NEEDLE_HOME/state/heartbeats"
echo '{"current_bead": "", "status": "idle"}' > "$heartbeat_dir/test-session.json"

if _needle_wait_for_idle "test-session" 5 2>/dev/null; then
    test_pass
else
    test_fail "Should return 0 when current_bead is empty"
fi

# Restore real _needle_session_exists
unset -f _needle_session_exists
source "$PROJECT_DIR/src/runner/tmux.sh" 2>/dev/null

test_case "_needle_wait_for_idle: returns 0 when current_bead is null"
setup_state_dir

# Mock session as existing with heartbeat showing null bead
_needle_session_exists() { return 0; }

heartbeat_dir="$TEST_NEEDLE_HOME/state/heartbeats"
echo '{"current_bead": null, "status": "idle"}' > "$heartbeat_dir/test-session.json"

if _needle_wait_for_idle "test-session" 5 2>/dev/null; then
    test_pass
else
    test_fail "Should return 0 when current_bead is null"
fi

# Restore real _needle_session_exists
unset -f _needle_session_exists
source "$PROJECT_DIR/src/runner/tmux.sh" 2>/dev/null

test_case "_needle_wait_for_idle: returns 1 on timeout when bead is active"
setup_state_dir

# Mock session as always existing with active bead
_needle_session_exists() { return 0; }

heartbeat_dir="$TEST_NEEDLE_HOME/state/heartbeats"
echo '{"current_bead": "nd-abc", "status": "executing"}' > "$heartbeat_dir/busy-session.json"

# Use very short timeout (1 second) and short check interval won't matter,
# override check_interval by testing with 1s timeout
if ! _needle_wait_for_idle "busy-session" 1 2>/dev/null; then
    test_pass
else
    test_fail "Should return 1 when timeout reached with active bead"
fi

# Restore real _needle_session_exists
unset -f _needle_session_exists
source "$PROJECT_DIR/src/runner/tmux.sh" 2>/dev/null

# ---- _needle_get_worker_config Tests ----

test_case "_needle_get_worker_config: retrieves worker from registry"
setup_state_dir

# Register a test worker
cat > "$NEEDLE_WORKERS_FILE" << 'EOF'
{"workers":[
  {
    "session": "needle-claude-anthropic-sonnet-alpha",
    "runner": "claude",
    "provider": "anthropic",
    "model": "sonnet",
    "identifier": "alpha",
    "pid": 99999,
    "workspace": "/home/test",
    "started": "2026-03-08T00:00:00Z"
  }
]}
EOF

result=$(_needle_get_worker_config "needle-claude-anthropic-sonnet-alpha" 2>/dev/null)
if [[ -n "$result" ]] && [[ "$result" != "{}" ]]; then
    runner=$(echo "$result" | jq -r '.runner // ""' 2>/dev/null)
    if [[ "$runner" == "claude" ]]; then
        test_pass
    else
        test_fail "Expected runner=claude, got: $runner"
    fi
else
    test_fail "Should retrieve worker config from registry"
fi

test_case "_needle_get_worker_config: falls back to parsing session name"
setup_state_dir

# Empty registry
echo '{"workers":[]}' > "$NEEDLE_WORKERS_FILE"

result=$(_needle_get_worker_config "needle-claude-anthropic-sonnet-bravo" 2>/dev/null)
if [[ -n "$result" ]] && [[ "$result" != "{}" ]]; then
    runner=$(echo "$result" | jq -r '.runner // ""' 2>/dev/null)
    provider=$(echo "$result" | jq -r '.provider // ""' 2>/dev/null)
    model=$(echo "$result" | jq -r '.model // ""' 2>/dev/null)
    if [[ "$runner" == "claude" ]] && [[ "$provider" == "anthropic" ]] && [[ "$model" == "sonnet" ]]; then
        test_pass
    else
        test_fail "Expected claude/anthropic/sonnet, got: $runner/$provider/$model"
    fi
else
    test_fail "Should parse config from session name: result='$result'"
fi

test_case "_needle_get_worker_config: returns empty for unparseable session"
setup_state_dir

echo '{"workers":[]}' > "$NEEDLE_WORKERS_FILE"

# "badname" has no dashes so parse fails: runner="badname", provider="", model=""
result=$(_needle_get_worker_config "badname" 2>/dev/null)
if [[ "$result" == "{}" ]]; then
    test_pass
else
    test_fail "Should return empty object for unparseable session, got: $result"
fi

# ---- _needle_respawn_worker Tests ----

test_case "_needle_respawn_worker: fails when config missing required fields"
setup_state_dir

bad_config='{"runner": "", "provider": "", "model": "", "workspace": ""}'
if ! _needle_respawn_worker "needle-old-session" "$bad_config" 2>/dev/null; then
    test_pass
else
    test_fail "Should fail when runner/provider/model are empty"
fi

test_case "_needle_respawn_worker: attempts to create new session with valid config"
setup_state_dir

# Mock _needle_next_identifier and _needle_create_session
_needle_next_identifier() { echo "bravo"; }
_needle_create_session() {
    NEEDLE_RESTARTED_SESSION="needle-claude-anthropic-sonnet-bravo"
    return 0
}

valid_config='{"runner": "claude", "provider": "anthropic", "model": "sonnet", "identifier": "alpha", "workspace": "/home/test", "agent": "claude-anthropic-sonnet"}'
if _needle_respawn_worker "needle-claude-anthropic-sonnet-alpha" "$valid_config" 2>/dev/null; then
    if [[ -n "$NEEDLE_RESTARTED_SESSION" ]]; then
        test_pass
    else
        test_fail "NEEDLE_RESTARTED_SESSION should be set after respawn"
    fi
else
    test_fail "Should succeed with valid config"
fi

# Restore real functions
unset -f _needle_next_identifier
unset -f _needle_create_session
source "$PROJECT_DIR/src/runner/tmux.sh" 2>/dev/null

# ---- _needle_restart: no workers found ----

test_case "_needle_restart --all: reports no workers when none exist"
setup_state_dir

# Run in subshell so exit inside _needle_restart doesn't kill test process
# Mock _needle_list_sessions inside the subshell to return empty
(
    _needle_list_sessions() { true; }
    _needle_restart --all 2>/dev/null
)
exit_code=$?

# Should exit successfully (no workers is OK)
if [[ $exit_code -eq 0 ]]; then
    test_pass
else
    test_fail "Should exit 0 when no workers found with --all (got exit code $exit_code)"
fi

test_case "_needle_restart specific worker: reports error when worker not found"
setup_state_dir

# Mock tmux to return no sessions
_needle_session_exists() { return 1; }
_needle_list_sessions() { echo ""; return 0; }

(
    _needle_restart nonexistent-worker 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?

if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Should fail when specified worker not found"
fi

# Restore real functions
unset -f _needle_session_exists
unset -f _needle_list_sessions
source "$PROJECT_DIR/src/runner/tmux.sh" 2>/dev/null

# ---- Shutdown directory creation ----

test_case "_needle_restart creates shutdown directory if missing"
# Remove shutdown dir to test auto-creation
rm -rf "$NEEDLE_SHUTDOWN_DIR"

# Mock functions so we don't actually need workers
_needle_list_sessions() { echo ""; return 0; }

_needle_restart --all 2>/dev/null || true

if [[ -d "$NEEDLE_SHUTDOWN_DIR" ]]; then
    test_pass
else
    test_fail "Should create shutdown directory"
fi

# Restore real _needle_list_sessions
unset -f _needle_list_sessions
source "$PROJECT_DIR/src/runner/tmux.sh" 2>/dev/null

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
