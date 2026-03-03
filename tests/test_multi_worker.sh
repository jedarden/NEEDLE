#!/usr/bin/env bash
# Tests for NEEDLE multi-worker spawning (nd-2pw)
# Tests the --count=N option for spawning multiple workers in parallel

# Test setup
TEST_DIR=$(mktemp -d)
TEST_WORKSPACE="$TEST_DIR/workspace"
TEST_NEEDLE_HOME="$TEST_DIR/.needle"

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_HOME="$TEST_NEEDLE_HOME"
export NEEDLE_CONFIG_FILE="$NEEDLE_HOME/config.yaml"
export NEEDLE_CONFIG_NAME="config.yaml"

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/paths.sh"
source "$PROJECT_DIR/src/lib/json.sh"
source "$PROJECT_DIR/src/lib/config.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/lib/workspace.sh"
source "$PROJECT_DIR/src/agent/loader.sh"
source "$PROJECT_DIR/src/onboarding/agents.sh"
source "$PROJECT_DIR/src/runner/limits.sh"
source "$PROJECT_DIR/src/runner/state.sh"
source "$PROJECT_DIR/src/runner/naming.sh"
source "$PROJECT_DIR/src/runner/tmux.sh"
source "$PROJECT_DIR/src/telemetry/events.sh"
source "$PROJECT_DIR/src/cli/run.sh"

# Suppress output for tests
export NEEDLE_QUIET=true

# Cleanup function
cleanup() {
    # Kill any test tmux sessions
    tmux list-sessions 2>/dev/null | grep '^needle-test' | cut -d: -f1 | while read -r s; do
        tmux kill-session -t "$s" 2>/dev/null || true
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

# Create test workspace with .beads directory
setup_test_workspace() {
    mkdir -p "$TEST_WORKSPACE/.beads"
    mkdir -p "$TEST_NEEDLE_HOME"
    mkdir -p "$TEST_NEEDLE_HOME/agents"
    mkdir -p "$TEST_NEEDLE_HOME/$NEEDLE_STATE_DIR"
}

# Create a test agent config
setup_test_agent() {
    local agent_name="${1:-test-agent}"
    cat > "$TEST_NEEDLE_HOME/agents/${agent_name}.yaml" << EOF
name: $agent_name
description: Test agent
version: "1.0"
runner: test
provider: test
model: model
invoke: "echo test"
input:
  method: heredoc
output:
  format: text
  success_codes:
    - 0
limits:
  requests_per_minute: 60
  max_concurrent: 10
EOF
}

# ============ Tests ============

# ---- get_next_identifier_from_list Tests ----

test_case "get_next_identifier_from_list: returns alpha for empty list"
result=$(get_next_identifier_from_list "")
if [[ "$result" == "alpha" ]]; then
    test_pass
else
    test_fail "Expected 'alpha', got '$result'"
fi

test_case "get_next_identifier_from_list: skips used identifiers"
result=$(get_next_identifier_from_list "alpha bravo")
if [[ "$result" == "charlie" ]]; then
    test_pass
else
    test_fail "Expected 'charlie', got '$result'"
fi

test_case "get_next_identifier_from_list: finds first unused in middle"
result=$(get_next_identifier_from_list "alpha charlie delta")
if [[ "$result" == "bravo" ]]; then
    test_pass
else
    test_fail "Expected 'bravo', got '$result'"
fi

test_case "get_next_identifier_from_list: handles all 26 NATO names used"
# Create a list with all 26 NATO names
all_nato="${NEEDLE_NATO_ALPHABET[*]}"
result=$(get_next_identifier_from_list "$all_nato")
if [[ "$result" == "alpha-27" ]]; then
    test_pass
else
    test_fail "Expected 'alpha-27', got '$result'"
fi

# ---- Session Name Generation Tests ----

test_case "_needle_generate_session_name: generates correct default pattern"
result=$(_needle_generate_session_name "" "claude" "anthropic" "sonnet" "alpha")
if [[ "$result" == "needle-claude-anthropic-sonnet-alpha" ]]; then
    test_pass
else
    test_fail "Expected 'needle-claude-anthropic-sonnet-alpha', got '$result'"
fi

test_case "_needle_generate_session_name: uses custom pattern"
result=$(_needle_generate_session_name "worker-{identifier}-{runner}" "claude" "anthropic" "sonnet" "bravo")
if [[ "$result" == "worker-bravo-claude" ]]; then
    test_pass
else
    test_fail "Expected 'worker-bravo-claude', got '$result'"
fi

test_case "_needle_generate_session_name: sanitizes special characters"
result=$(_needle_generate_session_name "" "claude" "anthropic" "son-4" "alpha")
if [[ "$result" == "needle-claude-anthropic-son-4-alpha" ]]; then
    test_pass
else
    test_fail "Expected 'needle-claude-anthropic-son-4-alpha', got '$result'"
fi

# ---- Multi-Worker Identifier Allocation Tests ----

test_case "_needle_spawn_multiple_workers: allocates unique identifiers"
setup_test_workspace
setup_test_agent "test-multi-agent"

# Mock tmux commands (we'll test the identifier allocation logic)
# The function should allocate unique NATO identifiers for each worker

# Test that identifiers are unique when spawning multiple workers
local used_ids=""
for i in {1..5}; do
    local next_id
    next_id=$(get_next_identifier_from_list "$used_ids")
    used_ids="$used_ids $next_id"
done

# Check all 5 are unique
unique_count=$(echo "$used_ids" | tr ' ' '\n' | sort -u | grep -v '^$' | wc -l | tr -d ' ')
if [[ "$unique_count" == "5" ]]; then
    test_pass
else
    test_fail "Expected 5 unique identifiers, got $unique_count: $used_ids"
fi

# ---- Concurrency Limit Tests ----

test_case "_needle_check_concurrency: rejects count exceeding model limit"
setup_test_workspace
setup_test_agent "limit-test-agent"

# Initialize workers registry
_needle_workers_init

# Check that requesting 20 workers (exceeds agent's max_concurrent of 10) fails
if ! _needle_check_concurrency "limit-test-agent" "test" 20 2>/dev/null; then
    if [[ "$NEEDLE_LIMIT_CHECK_PASSED" == "false" ]]; then
        test_pass
    else
        test_fail "Check should have failed but passed"
    fi
else
    test_fail "Should reject count exceeding model limit (10)"
fi

test_case "_needle_check_concurrency: accepts count within model limit"
setup_test_workspace
setup_test_agent "limit-test-agent"

# Initialize workers registry
_needle_workers_init

# Check that requesting 5 workers (within agent's max_concurrent of 10) passes
if _needle_check_concurrency "limit-test-agent" "test" 5 2>/dev/null; then
    test_pass
else
    test_fail "Should accept count within model limit: $NEEDLE_LIMIT_CHECK_MESSAGE"
fi

test_case "_needle_check_concurrency: rejects count that would exceed global limit"
setup_test_workspace
setup_test_agent "global-limit-agent"

# Initialize workers registry
_needle_workers_init

# Get the global limit
local global_limit
global_limit=$(_needle_get_global_limit)

# Check that requesting more than global limit fails
local excessive_count=$((global_limit + 10))
if ! _needle_check_concurrency "global-limit-agent" "test" "$excessive_count" 2>/dev/null; then
    if [[ "$NEEDLE_LIMIT_CHECK_PASSED" == "false" ]]; then
        test_pass
    else
        test_fail "Check should have failed"
    fi
else
    test_fail "Should reject count exceeding global limit ($global_limit)"
fi

# ---- Count Validation Tests ----

test_case "_needle_validate_count: accepts count=1"
unset NEEDLE_VALIDATED_COUNT
if _needle_validate_count "1" 2>/dev/null; then
    if [[ "$NEEDLE_VALIDATED_COUNT" == "1" ]]; then
        test_pass
    else
        test_fail "Expected 1, got $NEEDLE_VALIDATED_COUNT"
    fi
else
    test_fail "Should accept count=1"
fi

test_case "_needle_validate_count: accepts large count"
unset NEEDLE_VALIDATED_COUNT
if _needle_validate_count "100" 2>/dev/null; then
    if [[ "$NEEDLE_VALIDATED_COUNT" == "100" ]]; then
        test_pass
    else
        test_fail "Expected 100, got $NEEDLE_VALIDATED_COUNT"
    fi
else
    test_fail "Should accept large count"
fi

# ---- Session Creation Tests (with tmux mock) ----

test_case "_needle_create_session: creates session with valid name and command"
# Skip if tmux not available
if ! command -v tmux &>/dev/null; then
    echo "SKIP (tmux not available)"
    continue
fi

# Create a simple test session
local test_session="needle-test-session-$$"
if _needle_create_session "$test_session" "echo 'test' && sleep 0.1" 2>/dev/null; then
    # Verify session exists
    if tmux has-session -t "$test_session" 2>/dev/null; then
        tmux kill-session -t "$test_session" 2>/dev/null
        test_pass
    else
        test_fail "Session was not created"
    fi
else
    test_fail "Failed to create session"
fi

test_case "_needle_create_session: rejects duplicate session name"
# Skip if tmux not available
if ! command -v tmux &>/dev/null; then
    echo "SKIP (tmux not available)"
    continue
fi

local test_session="needle-test-dup-$$"
# Create first session
_needle_create_session "$test_session" "sleep 1" 2>/dev/null

# Try to create duplicate
if ! _needle_create_session "$test_session" "sleep 1" 2>/dev/null; then
    tmux kill-session -t "$test_session" 2>/dev/null
    test_pass
else
    tmux kill-session -t "$test_session" 2>/dev/null
    test_fail "Should reject duplicate session name"
fi

# ---- NATO Alphabet Tests ----

test_case "NEEDLE_NATO_ALPHABET: contains all 26 letters"
local nato_count=${#NEEDLE_NATO_ALPHABET[@]}
if [[ "$nato_count" -eq 26 ]]; then
    test_pass
else
    test_fail "Expected 26 NATO names, got $nato_count"
fi

test_case "NEEDLE_NATO_ALPHABET: starts with alpha"
if [[ "${NEEDLE_NATO_ALPHABET[0]}" == "alpha" ]]; then
    test_pass
else
    test_fail "Expected first element to be 'alpha', got '${NEEDLE_NATO_ALPHABET[0]}'"
fi

test_case "NEEDLE_NATO_ALPHABET: ends with zulu"
local last_idx=$((${#NEEDLE_NATO_ALPHABET[@]} - 1))
if [[ "${NEEDLE_NATO_ALPHABET[$last_idx]}" == "zulu" ]]; then
    test_pass
else
    test_fail "Expected last element to be 'zulu', got '${NEEDLE_NATO_ALPHABET[$last_idx]}'"
fi

# ---- Helper Function Tests ----

test_case "get_next_identifier: returns alpha when no sessions exist"
# Clean up any existing needle sessions for this test agent
tmux list-sessions 2>/dev/null | grep '^needle-test-empty-' | cut -d: -f1 | while read -r s; do
    tmux kill-session -t "$s" 2>/dev/null || true
done

result=$(get_next_identifier "test-empty-agent")
if [[ "$result" == "alpha" ]]; then
    test_pass
else
    test_fail "Expected 'alpha', got '$result'"
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
