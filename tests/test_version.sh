#!/usr/bin/env bash
# Tests for NEEDLE CLI version command (src/cli/version.sh)
#
# Tests the needle version command for displaying NEEDLE and dependency info.

# Test setup
TEST_DIR=$(mktemp -d)

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_HOME="$TEST_DIR/.needle"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=false

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/lib/json.sh"
source "$PROJECT_DIR/src/cli/version.sh"

# Cleanup function
cleanup() {
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

# Test counters
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

# ============================================================================
# Tests
# ============================================================================

echo "=== Version Command Tests ==="
echo ""

# ---- Help Tests ----

test_case "_needle_version_help runs without error"
if _needle_version_help >/dev/null 2>&1; then
    test_pass
else
    test_fail "Help function failed"
fi

test_case "_needle_version_help outputs main description"
help_output=$(_needle_version_help 2>&1)
if [[ "$help_output" == *"Show NEEDLE version information"* ]]; then
    test_pass
else
    test_fail "Missing main description"
fi

test_case "_needle_version_help contains USAGE section"
help_output=$(_needle_version_help 2>&1)
if [[ "$help_output" == *"USAGE:"* ]]; then
    test_pass
else
    test_fail "Missing USAGE section"
fi

test_case "_needle_version_help contains needle version in usage"
help_output=$(_needle_version_help 2>&1)
if [[ "$help_output" == *"needle version"* ]]; then
    test_pass
else
    test_fail "Missing 'needle version' in USAGE"
fi

test_case "_needle_version_help shows --json option"
help_output=$(_needle_version_help 2>&1)
if [[ "$help_output" == *"--json"* ]]; then
    test_pass
else
    test_fail "Missing --json option"
fi

test_case "_needle_version_help shows --short option"
help_output=$(_needle_version_help 2>&1)
if [[ "$help_output" == *"--short"* ]]; then
    test_pass
else
    test_fail "Missing --short option"
fi

test_case "_needle_version_help shows --help option"
help_output=$(_needle_version_help 2>&1)
if [[ "$help_output" == *"--help"* ]] || [[ "$help_output" == *"-h"* ]]; then
    test_pass
else
    test_fail "Missing --help option"
fi

test_case "_needle_version_help contains EXAMPLES section"
help_output=$(_needle_version_help 2>&1)
if [[ "$help_output" == *"EXAMPLES"* ]]; then
    test_pass
else
    test_fail "Missing EXAMPLES section"
fi

# ---- Argument Parsing Tests ----
# NOTE: _needle_version calls exit() so must run in a subshell

test_case "_needle_version: --help exits successfully"
(
    _needle_version --help 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?
if [[ $exit_code -eq 0 ]]; then
    test_pass
else
    test_fail "Help should exit successfully (got $exit_code)"
fi

test_case "_needle_version: -h exits successfully"
(
    _needle_version -h 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?
if [[ $exit_code -eq 0 ]]; then
    test_pass
else
    test_fail "Short help flag should exit successfully (got $exit_code)"
fi

test_case "_needle_version: rejects unknown option"
(
    _needle_version --unknown-flag 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?
if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Should reject unknown option"
fi

test_case "_needle_version: unknown option returns NEEDLE_EXIT_USAGE"
(
    _needle_version --unknown-flag 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?
if [[ $exit_code -eq $NEEDLE_EXIT_USAGE ]]; then
    test_pass
else
    test_fail "Should return NEEDLE_EXIT_USAGE (got $exit_code)"
fi

# ---- NEEDLE_VERSION output ----

test_case "_needle_version output includes NEEDLE_VERSION"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_text 2>&1
)
if echo "$output" | grep -q "$NEEDLE_VERSION"; then
    test_pass
else
    test_fail "Version output should include NEEDLE_VERSION ($NEEDLE_VERSION)"
fi

test_case "_needle_version --short outputs just version number"
(
    _needle_version --short 2>/dev/null
    exit $?
) > "$TEST_DIR/short_version" 2>&1
exit_code=$?
short_output=$(cat "$TEST_DIR/short_version")
if [[ "$short_output" == "$NEEDLE_VERSION" ]] && [[ $exit_code -eq 0 ]]; then
    test_pass
else
    test_fail "Expected '$NEEDLE_VERSION', got '$short_output' (exit $exit_code)"
fi

test_case "_needle_version -s flag works as short form of --short"
(
    _needle_version -s 2>/dev/null
    exit $?
) > "$TEST_DIR/short_version_s" 2>&1
exit_code=$?
short_output=$(cat "$TEST_DIR/short_version_s")
if [[ "$short_output" == "$NEEDLE_VERSION" ]] && [[ $exit_code -eq 0 ]]; then
    test_pass
else
    test_fail "Expected '$NEEDLE_VERSION' with -s, got '$short_output' (exit $exit_code)"
fi

test_case "VERSION file matches NEEDLE_VERSION constant"
if [[ -f "$PROJECT_DIR/VERSION" ]]; then
    file_version=$(cat "$PROJECT_DIR/VERSION" | tr -d '[:space:]')
    if [[ "$file_version" == "$NEEDLE_VERSION" ]]; then
        test_pass
    else
        test_fail "VERSION file ($file_version) doesn't match NEEDLE_VERSION ($NEEDLE_VERSION)"
    fi
else
    test_fail "VERSION file not found at $PROJECT_DIR/VERSION"
fi

# ---- --json flag ----

test_case "_needle_version --json outputs valid JSON"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
if echo "$output" | jq -e '.' >/dev/null 2>&1; then
    test_pass
else
    test_fail "JSON output is not valid JSON"
fi

test_case "_needle_version --json includes needle.version field"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
version=$(echo "$output" | jq -r '.needle.version' 2>/dev/null)
if [[ "$version" == "$NEEDLE_VERSION" ]]; then
    test_pass
else
    test_fail "Expected needle.version=$NEEDLE_VERSION, got: $version"
fi

test_case "_needle_version --json includes needle.major field"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
major=$(echo "$output" | jq -r '.needle.major' 2>/dev/null)
if [[ "$major" == "$NEEDLE_VERSION_MAJOR" ]]; then
    test_pass
else
    test_fail "Expected needle.major=$NEEDLE_VERSION_MAJOR, got: $major"
fi

test_case "_needle_version --json includes needle.minor field"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
minor=$(echo "$output" | jq -r '.needle.minor' 2>/dev/null)
if [[ "$minor" == "$NEEDLE_VERSION_MINOR" ]]; then
    test_pass
else
    test_fail "Expected needle.minor=$NEEDLE_VERSION_MINOR, got: $minor"
fi

test_case "_needle_version --json includes needle.patch field"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
patch=$(echo "$output" | jq -r '.needle.patch' 2>/dev/null)
if [[ "$patch" == "$NEEDLE_VERSION_PATCH" ]]; then
    test_pass
else
    test_fail "Expected needle.patch=$NEEDLE_VERSION_PATCH, got: $patch"
fi

test_case "_needle_version --json includes needle.repo field"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
if echo "$output" | jq -e '.needle.repo' >/dev/null 2>&1; then
    test_pass
else
    test_fail "Missing needle.repo in JSON output"
fi

test_case "_needle_version --json includes dependencies field"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
if echo "$output" | jq -e '.dependencies' >/dev/null 2>&1; then
    test_pass
else
    test_fail "Missing dependencies in JSON output"
fi

test_case "_needle_version --json includes agents field"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
if echo "$output" | jq -e '.agents' >/dev/null 2>&1; then
    test_pass
else
    test_fail "Missing agents in JSON output"
fi

test_case "_needle_version --json includes paths field"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
if echo "$output" | jq -e '.paths' >/dev/null 2>&1; then
    test_pass
else
    test_fail "Missing paths in JSON output"
fi

# ---- Dependency versions ----

test_case "_needle_version_text shows Dependencies section"
# _needle_section is suppressed by NEEDLE_QUIET, use NEEDLE_QUIET=false
output=$(
    NEEDLE_QUIET=false
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    NEEDLE_QUIET=false _needle_version_text 2>&1
)
if echo "$output" | grep -q "Dependencies:"; then
    test_pass
else
    test_fail "Missing 'Dependencies:' section in version text output"
fi

test_case "_needle_version_text lists tmux dependency"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_text 2>&1
)
if echo "$output" | grep -q "tmux"; then
    test_pass
else
    test_fail "Missing 'tmux' in dependencies output"
fi

test_case "_needle_version_text lists jq dependency"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_text 2>&1
)
if echo "$output" | grep -q "jq"; then
    test_pass
else
    test_fail "Missing 'jq' in dependencies output"
fi

test_case "_needle_version_text lists yq dependency"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_text 2>&1
)
if echo "$output" | grep -q "yq"; then
    test_pass
else
    test_fail "Missing 'yq' in dependencies output"
fi

test_case "_needle_version_text lists br (bead runner) dependency"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_text 2>&1
)
if echo "$output" | grep -q "br"; then
    test_pass
else
    test_fail "Missing 'br' in dependencies output"
fi

test_case "_needle_version_text shows Agents section"
output=$(
    NEEDLE_QUIET=false
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    NEEDLE_QUIET=false _needle_version_text 2>&1
)
if echo "$output" | grep -q "Agents:"; then
    test_pass
else
    test_fail "Missing 'Agents:' section in version text output"
fi

test_case "_needle_version_text shows Paths section"
output=$(
    NEEDLE_QUIET=false
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    NEEDLE_QUIET=false _needle_version_text 2>&1
)
if echo "$output" | grep -q "Paths:"; then
    test_pass
else
    test_fail "Missing 'Paths:' section in version text output"
fi

# ---- _needle_check_dep_version helper ----

test_case "_needle_check_dep_version returns - for non-existent command"
result=$(_needle_check_dep_version "nonexistent-command-xyz" 2>&1)
if [[ "$result" == "-" ]]; then
    test_pass
else
    test_fail "Expected '-' for missing command, got: '$result'"
fi

test_case "_needle_check_dep_version returns version for existing command"
result=$(_needle_check_dep_version "bash" "--version" 2>&1)
# Should return a version number or "unknown", not "-"
if [[ "$result" != "-" ]]; then
    test_pass
else
    test_fail "Expected version string for bash, got '-'"
fi

test_case "_needle_check_dep_version uses --version flag by default"
# Test with a command that supports --version
result=$(_needle_check_dep_version "bash" 2>&1)
if [[ "$result" != "-" ]]; then
    test_pass
else
    test_fail "Expected version string using default --version flag"
fi

# ---- _needle_json_value helper ----

test_case "_needle_json_value returns null for empty string"
result=$(_needle_json_value "" 2>&1)
if [[ "$result" == "null" ]]; then
    test_pass
else
    test_fail "Expected 'null' for empty string, got: '$result'"
fi

test_case "_needle_json_value returns null for '-'"
result=$(_needle_json_value "-" 2>&1)
if [[ "$result" == "null" ]]; then
    test_pass
else
    test_fail "Expected 'null' for '-', got: '$result'"
fi

test_case "_needle_json_value returns null for 'null'"
result=$(_needle_json_value "null" 2>&1)
if [[ "$result" == "null" ]]; then
    test_pass
else
    test_fail "Expected 'null' for 'null' input, got: '$result'"
fi

test_case "_needle_json_value returns quoted string for version"
result=$(_needle_json_value "1.2.3" 2>&1)
if [[ "$result" == '"1.2.3"' ]]; then
    test_pass
else
    test_fail "Expected '\"1.2.3\"', got: '$result'"
fi

# ---- _needle_count_log_sessions helper ----

test_case "_needle_count_log_sessions returns 0 when log dir missing"
export NEEDLE_HOME="$TEST_DIR/no-home"
result=$(_needle_count_log_sessions 2>&1)
export NEEDLE_HOME="$TEST_DIR/.needle"
if [[ "$result" == "0" ]]; then
    test_pass
else
    test_fail "Expected 0 for missing log dir, got: '$result'"
fi

test_case "_needle_count_log_sessions returns correct count for log files"
mkdir -p "$NEEDLE_HOME/$NEEDLE_LOG_DIR"
touch "$NEEDLE_HOME/$NEEDLE_LOG_DIR/session1.log"
touch "$NEEDLE_HOME/$NEEDLE_LOG_DIR/session2.log"
touch "$NEEDLE_HOME/$NEEDLE_LOG_DIR/session3.log"
result=$(_needle_count_log_sessions 2>&1)
if [[ "$result" == "3" ]]; then
    test_pass
else
    test_fail "Expected 3 log sessions, got: '$result'"
fi
# Cleanup
rm -f "$NEEDLE_HOME/$NEEDLE_LOG_DIR/session1.log" \
      "$NEEDLE_HOME/$NEEDLE_LOG_DIR/session2.log" \
      "$NEEDLE_HOME/$NEEDLE_LOG_DIR/session3.log"

# ---- JSON dependency structure ----

test_case "_needle_version_json dependencies include tmux with status"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
if echo "$output" | jq -e '.dependencies.tmux.status' >/dev/null 2>&1; then
    test_pass
else
    test_fail "Missing dependencies.tmux.status in JSON"
fi

test_case "_needle_version_json dependencies include jq with status"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
if echo "$output" | jq -e '.dependencies.jq.status' >/dev/null 2>&1; then
    test_pass
else
    test_fail "Missing dependencies.jq.status in JSON"
fi

test_case "_needle_version_json agents include claude entry"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
if echo "$output" | jq -e '.agents.claude' >/dev/null 2>&1; then
    test_pass
else
    test_fail "Missing agents.claude in JSON"
fi

test_case "_needle_version_json paths include config field"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
if echo "$output" | jq -e '.paths.config' >/dev/null 2>&1; then
    test_pass
else
    test_fail "Missing paths.config in JSON"
fi

test_case "_needle_version_json paths include logs field"
output=$(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version_json 2>&1
)
if echo "$output" | jq -e '.paths.logs' >/dev/null 2>&1; then
    test_pass
else
    test_fail "Missing paths.logs in JSON"
fi

# ---- Full command integration ----

test_case "_needle_version exits successfully with no args"
(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?
if [[ $exit_code -eq 0 ]]; then
    test_pass
else
    test_fail "Should exit 0 with no args (got $exit_code)"
fi

test_case "_needle_version --json exits successfully"
(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version --json 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?
if [[ $exit_code -eq 0 ]]; then
    test_pass
else
    test_fail "Should exit 0 with --json flag (got $exit_code)"
fi

test_case "_needle_version -j flag accepted as short form of --json"
(
    _needle_agent_version() { echo "unknown"; }
    _needle_agent_auth_status() { echo "unknown"; }
    _needle_version -j 2>/dev/null
    exit $?
) >/dev/null 2>&1
exit_code=$?
if [[ $exit_code -eq 0 ]]; then
    test_pass
else
    test_fail "Should accept -j flag (got $exit_code)"
fi

# ============================================================================
# Summary
# ============================================================================

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
