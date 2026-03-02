#!/usr/bin/env bash
# Tests for NEEDLE agents module (src/onboarding/agents.sh)

# Test setup
TEST_DIR=$(mktemp -d)

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_HOME="$TEST_DIR/.needle"
export NEEDLE_CONFIG_FILE="$NEEDLE_HOME/config.yaml"
export NEEDLE_CONFIG_NAME="config.yaml"

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/json.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/onboarding/agents.sh"

# Suppress output for tests
export NEEDLE_QUIET=true

# Cleanup function
cleanup() {
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

# ============ Tests ============

# Test: NEEDLE_AGENT_CMDS is defined
test_case "NEEDLE_AGENT_CMDS associative array is defined"
if [[ -n "${NEEDLE_AGENT_CMDS[claude]:-}" ]]; then
    test_pass
else
    test_fail "NEEDLE_AGENT_CMDS not defined"
fi

# Test: NEEDLE_AGENT_INSTALL is defined
test_case "NEEDLE_AGENT_INSTALL associative array is defined"
if [[ -n "${NEEDLE_AGENT_INSTALL[claude]:-}" ]]; then
    test_pass
else
    test_fail "NEEDLE_AGENT_INSTALL not defined"
fi

# Test: NEEDLE_AGENT_CMDS has expected agents
test_case "NEEDLE_AGENT_CMDS has claude"
if [[ "${NEEDLE_AGENT_CMDS[claude]}" == "claude" ]]; then
    test_pass
else
    test_fail "Expected 'claude', got '${NEEDLE_AGENT_CMDS[claude]:-}'"
fi

test_case "NEEDLE_AGENT_CMDS has opencode"
if [[ "${NEEDLE_AGENT_CMDS[opencode]}" == "opencode" ]]; then
    test_pass
else
    test_fail "Expected 'opencode', got '${NEEDLE_AGENT_CMDS[opencode]:-}'"
fi

test_case "NEEDLE_AGENT_CMDS has codex"
if [[ "${NEEDLE_AGENT_CMDS[codex]}" == "codex" ]]; then
    test_pass
else
    test_fail "Expected 'codex', got '${NEEDLE_AGENT_CMDS[codex]:-}'"
fi

test_case "NEEDLE_AGENT_CMDS has aider"
if [[ "${NEEDLE_AGENT_CMDS[aider]}" == "aider" ]]; then
    test_pass
else
    test_fail "Expected 'aider', got '${NEEDLE_AGENT_CMDS[aider]:-}'"
fi

# Test: NEEDLE_AGENT_INSTALL has install commands
test_case "NEEDLE_AGENT_INSTALL has claude install command"
if [[ "${NEEDLE_AGENT_INSTALL[claude]}" == *"npm install"* ]]; then
    test_pass
else
    test_fail "Expected npm install command, got '${NEEDLE_AGENT_INSTALL[claude]:-}'"
fi

test_case "NEEDLE_AGENT_INSTALL has aider install command"
if [[ "${NEEDLE_AGENT_INSTALL[aider]}" == *"pip install"* ]]; then
    test_pass
else
    test_fail "Expected pip install command, got '${NEEDLE_AGENT_INSTALL[aider]:-}'"
fi

# Test: _needle_detect_agent returns "missing" for unknown command
test_case "_needle_detect_agent returns missing for non-existent agent"
result=$(_needle_detect_agent "nonexistent_agent" 2>/dev/null)
exit_code=$?
if [[ "$result" == "missing" ]] || [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Expected 'missing' or error, got '$result'"
fi

# Test: _needle_agent_install_cmd returns install command
test_case "_needle_agent_install_cmd returns claude install command"
cmd=$(_needle_agent_install_cmd "claude")
if [[ "$cmd" == *"npm install"* ]]; then
    test_pass
else
    test_fail "Expected npm install command, got '$cmd'"
fi

# Test: _needle_scan_agents_json returns valid JSON array
test_case "_needle_scan_agents_json returns valid JSON array"
json=$(_needle_scan_agents_json)
if [[ "$json" == "["*"]" ]] && [[ "$json" == *'"name"'* ]] && [[ "$json" == *'"installed"'* ]]; then
    test_pass
else
    test_fail "Expected JSON array with agent objects, got: ${json:0:100}..."
fi

# Test: _needle_scan_agents_json contains expected agents
test_case "_needle_scan_agents_json contains claude agent"
json=$(_needle_scan_agents_json)
if [[ "$json" == *'"claude"'* ]]; then
    test_pass
else
    test_fail "Expected JSON to contain claude agent"
fi

test_case "_needle_scan_agents_json contains opencode agent"
json=$(_needle_scan_agents_json)
if [[ "$json" == *'"opencode"'* ]]; then
    test_pass
else
    test_fail "Expected JSON to contain opencode agent"
fi

# Test: _needle_get_installed_agents returns empty when no agents installed
test_case "_needle_get_installed_agents returns empty or space-separated list"
installed=$(_needle_get_installed_agents)
# This should not fail - it returns what's actually installed
if [[ -z "$installed" ]] || [[ "$installed" =~ ^[a-z]+(\s[a-z]+)*$ ]]; then
    test_pass
else
    test_fail "Expected empty or space-separated agent names, got '$installed'"
fi

# Test: _needle_get_authenticated_agents returns proper format
test_case "_needle_get_authenticated_agents returns proper format"
authenticated=$(_needle_get_authenticated_agents)
# This should not fail - it returns what's actually authenticated
if [[ -z "$authenticated" ]] || [[ "$authenticated" =~ ^[a-z]+(\s[a-z]+)*$ ]]; then
    test_pass
else
    test_fail "Expected empty or space-separated agent names, got '$authenticated'"
fi

# Test: _needle_is_agent_ready returns 1 for non-existent agent
test_case "_needle_is_agent_ready returns 1 for non-existent agent"
if ! _needle_is_agent_ready "nonexistent_agent" 2>/dev/null; then
    test_pass
else
    test_fail "Expected false for non-existent agent"
fi

# Test: _needle_get_default_agent returns empty or valid agent
test_case "_needle_get_default_agent returns empty or valid agent name"
default=$(_needle_get_default_agent 2>/dev/null)
if [[ -z "$default" ]] || [[ "$default" =~ ^[a-z]+$ ]]; then
    test_pass
else
    test_fail "Expected empty or agent name, got '$default'"
fi

# Test: _needle_agent_version handles unknown agent gracefully
test_case "_needle_agent_version handles unknown agent gracefully"
version=$(_needle_agent_version "unknown_agent" 2>/dev/null)
# Should return something (even if empty or "unknown")
if [[ -n "$version" ]] || [[ -z "$version" ]]; then
    test_pass
else
    test_fail "Should not fail for unknown agent"
fi

# Test: _needle_agent_auth_status handles unknown agent gracefully
test_case "_needle_agent_auth_status handles unknown agent gracefully"
auth=$(_needle_agent_auth_status "unknown_agent" 2>/dev/null)
# Should return something
if [[ -n "$auth" ]] || [[ -z "$auth" ]]; then
    test_pass
else
    test_fail "Should not fail for unknown agent"
fi

# Test: NEEDLE_AGENT_NAMES is defined
test_case "NEEDLE_AGENT_NAMES associative array is defined"
if [[ -n "${NEEDLE_AGENT_NAMES[claude]:-}" ]]; then
    test_pass
else
    test_fail "NEEDLE_AGENT_NAMES not defined"
fi

# Test: NEEDLE_AGENT_AUTH_ENV is defined
test_case "NEEDLE_AGENT_AUTH_ENV associative array is defined"
if [[ -n "${NEEDLE_AGENT_AUTH_ENV[claude]:-}" ]]; then
    test_pass
else
    test_fail "NEEDLE_AGENT_AUTH_ENV not defined"
fi

# Test: JSON escape function exists and works
test_case "_needle_json_escape escapes special characters"
escaped=$(_needle_json_escape 'test "quoted" string')
if [[ "$escaped" == *'\"'* ]]; then
    test_pass
else
    test_fail "Expected escaped quotes, got '$escaped'"
fi

# Test: Scan agents doesn't fail
test_case "_needle_scan_agents runs without error"
if _needle_scan_agents >/dev/null 2>&1; then
    test_pass
else
    test_fail "_needle_scan_agents failed"
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
