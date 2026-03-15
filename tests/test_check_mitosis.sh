#!/usr/bin/env bash
# Tests for _needle_check_mitosis
# Covers: input validation, enabled/disabled guard, type/label skip logic,
#         min_complexity gate, analysis → perform flow.

TEST_DIR=$(mktemp -d)
TEST_CONFIG_DIR="$TEST_DIR/.needle"
TEST_CONFIG_FILE="$TEST_CONFIG_DIR/config.yaml"
BR_LOG="$TEST_DIR/br_calls.log"
ANALYZE_LOG="$TEST_DIR/analyze_calls.log"
PERFORM_LOG="$TEST_DIR/perform_calls.log"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

export NEEDLE_HOME="$TEST_CONFIG_DIR"
export NEEDLE_CONFIG_FILE="$TEST_CONFIG_FILE"
export NEEDLE_CONFIG_NAME="config.yaml"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=false

source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/json.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/lib/config.sh"
source "$PROJECT_DIR/src/lib/workspace.sh"

cleanup() {
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
  max_children: 5
  min_children: 2
  min_complexity: 3
  skip_types: bug,hotfix
  skip_labels: no-mitosis,atomic
  timeout: 60
EOF

# ============================================================================
# Mock infrastructure
# ============================================================================

# Bead JSON returned by mock br
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"task","labels":[],"description":"line1\nline2\nline3\nline4\nline5","priority":2}'

# Analysis JSON returned by mock _needle_analyze_for_mitosis
MOCK_ANALYSIS_JSON='{"mitosis":true,"reasoning":"test","children":[{"title":"C1","description":"d1","affected_files":[],"verification_cmd":"","labels":[],"blocked_by":[]},{"title":"C2","description":"d2","affected_files":[],"verification_cmd":"","labels":[],"blocked_by":[]}]}'

# br mock
br() {
    echo "$*" >> "$BR_LOG"
    case "$1" in
        show) echo "$MOCK_BEAD_JSON" ;;
    esac
    return 0
}

# _needle_analyze_for_mitosis mock
_needle_analyze_for_mitosis() {
    echo "$*" >> "$ANALYZE_LOG"
    echo "$MOCK_ANALYSIS_JSON"
    return 0
}

# _needle_perform_mitosis mock — returns 0 by default
MOCK_PERFORM_RC=0
_needle_perform_mitosis() {
    echo "$*" >> "$PERFORM_LOG"
    return $MOCK_PERFORM_RC
}

_needle_emit_event() { return 0; }

# Source mitosis AFTER mocks (so it doesn't overwrite them)
source "$PROJECT_DIR/src/bead/mitosis.sh"

# Re-assert mocks (claim.sh sourced inside mitosis.sh may reset things)
br() {
    echo "$*" >> "$BR_LOG"
    case "$1" in
        show) echo "$MOCK_BEAD_JSON" ;;
    esac
    return 0
}
_needle_analyze_for_mitosis() {
    echo "$*" >> "$ANALYZE_LOG"
    echo "$MOCK_ANALYSIS_JSON"
    return 0
}
_needle_perform_mitosis() {
    echo "$*" >> "$PERFORM_LOG"
    return $MOCK_PERFORM_RC
}
_needle_emit_event() { return 0; }

# ============================================================================
# Helpers
# ============================================================================

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

WS_DIR=$(mktemp -d "$TEST_DIR/ws-XXXXXX")

test_case() {
    local name="$1"
    ((TESTS_RUN++))
    echo -n "Testing: $name... "
    rm -f "$BR_LOG" "$ANALYZE_LOG" "$PERFORM_LOG"
    touch "$BR_LOG" "$ANALYZE_LOG" "$PERFORM_LOG"
    NEEDLE_CONFIG_CACHE=""
    _NEEDLE_WORKSPACE_CACHE=()
    MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"task","labels":[],"description":"line1\nline2\nline3\nline4\nline5","priority":2}'
    MOCK_ANALYSIS_JSON='{"mitosis":true,"reasoning":"test","children":[{"title":"C1","description":"d1","affected_files":[],"verification_cmd":"","labels":[],"blocked_by":[]},{"title":"C2","description":"d2","affected_files":[],"verification_cmd":"","labels":[],"blocked_by":[]}]}'
    MOCK_PERFORM_RC=0
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
# Input validation
# ============================================================================

test_case "_needle_check_mitosis returns 1 when bead_id is empty"
_needle_check_mitosis "" "$WS_DIR" "agent" &>/dev/null
rc=$?
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit for empty bead_id"
fi

test_case "_needle_check_mitosis returns 1 when workspace is empty"
_needle_check_mitosis "nd-test" "" "agent" &>/dev/null
rc=$?
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit for empty workspace"
fi

# ============================================================================
# Enabled/disabled guard
# ============================================================================

test_case "_needle_check_mitosis returns 1 when mitosis disabled in global config"
NEEDLE_CONFIG_CACHE=""
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: false
  max_children: 5
  min_children: 2
  min_complexity: 3
  skip_types: bug,hotfix
  skip_labels: no-mitosis,atomic
EOF
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
rc=$?
# Restore config
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
  max_children: 5
  min_children: 2
  min_complexity: 3
  skip_types: bug,hotfix
  skip_labels: no-mitosis,atomic
  timeout: 60
EOF
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit when mitosis is disabled"
fi

test_case "_needle_check_mitosis returns 1 when mitosis disabled via workspace override"
ws_override=$(mktemp -d "$TEST_DIR/ws-override-XXXXXX")
cat > "$ws_override/.needle.yaml" << 'EOF'
mitosis:
  enabled: false
EOF
_needle_check_mitosis "nd-test" "$ws_override" "agent" &>/dev/null
rc=$?
rm -rf "$ws_override"
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit when mitosis disabled via workspace override"
fi

# ============================================================================
# bead retrieval failure
# ============================================================================

test_case "_needle_check_mitosis returns 1 when br show returns empty"
br() {
    echo "$*" >> "$BR_LOG"
    # Return nothing for show
    return 0
}
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
rc=$?
# Restore br mock
br() {
    echo "$*" >> "$BR_LOG"
    case "$1" in
        show) echo "$MOCK_BEAD_JSON" ;;
    esac
    return 0
}
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit when bead cannot be retrieved"
fi

# ============================================================================
# Type skip logic
# ============================================================================

test_case "_needle_check_mitosis skips bead of type 'bug' (in skip_types)"
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"bug","labels":[],"description":"line1\nline2\nline3\nline4\nline5","priority":2}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
rc=$?
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit for bead type 'bug'"
fi

test_case "_needle_check_mitosis skips bead of type 'hotfix' (in skip_types)"
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"hotfix","labels":[],"description":"line1\nline2\nline3\nline4\nline5","priority":2}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
rc=$?
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit for bead type 'hotfix'"
fi

test_case "_needle_check_mitosis does not skip bead of type 'task'"
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"task","labels":[],"description":"line1\nline2\nline3\nline4\nline5","priority":2}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
# 'task' is not in skip_types so it should proceed (return 0 if mitosis performed)
# We just verify analyze was called (meaning type check passed)
if grep -q "nd-test" "$ANALYZE_LOG" 2>/dev/null; then
    test_pass
else
    test_fail "Expected analysis to be called for bead type 'task'"
fi

# ============================================================================
# Label skip logic
# ============================================================================

test_case "_needle_check_mitosis skips bead with 'no-mitosis' label"
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"task","labels":["no-mitosis","backend"],"description":"line1\nline2\nline3\nline4\nline5","priority":2}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
rc=$?
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit for bead with 'no-mitosis' label"
fi

test_case "_needle_check_mitosis skips bead with 'atomic' label"
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"task","labels":["atomic"],"description":"line1\nline2\nline3\nline4\nline5","priority":2}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
rc=$?
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit for bead with 'atomic' label"
fi

test_case "_needle_check_mitosis does not skip bead with non-skip labels"
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"task","labels":["backend","security"],"description":"line1\nline2\nline3\nline4\nline5","priority":2}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
if grep -q "nd-test" "$ANALYZE_LOG" 2>/dev/null; then
    test_pass
else
    test_fail "Expected analysis to proceed for bead with non-skip labels"
fi

# ============================================================================
# Minimum complexity gate
# ============================================================================

test_case "_needle_check_mitosis skips bead with too-short description"
# min_complexity=3, so 2 lines should be rejected
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"task","labels":[],"description":"line1\nline2","priority":2}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
rc=$?
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit for description below min_complexity"
fi

test_case "_needle_check_mitosis passes bead meeting min_complexity"
# 5 lines > min_complexity=3
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"task","labels":[],"description":"line1\nline2\nline3\nline4\nline5","priority":2}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
if grep -q "nd-test" "$ANALYZE_LOG" 2>/dev/null; then
    test_pass
else
    test_fail "Expected analysis to be called for bead meeting min_complexity"
fi

# ============================================================================
# Analysis → perform flow
# ============================================================================

test_case "_needle_check_mitosis calls _needle_analyze_for_mitosis with bead_id"
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"task","labels":[],"description":"line1\nline2\nline3\nline4\nline5","priority":2}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
if grep -q "nd-test" "$ANALYZE_LOG"; then
    test_pass
else
    test_fail "Expected _needle_analyze_for_mitosis to be called with bead_id"
fi

test_case "_needle_check_mitosis calls _needle_perform_mitosis when analysis says split"
MOCK_ANALYSIS_JSON='{"mitosis":true,"reasoning":"split","children":[{"title":"C1","description":"d1","affected_files":[],"verification_cmd":"","labels":[],"blocked_by":[]},{"title":"C2","description":"d2","affected_files":[],"verification_cmd":"","labels":[],"blocked_by":[]}]}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
if [[ -s "$PERFORM_LOG" ]]; then
    test_pass
else
    test_fail "Expected _needle_perform_mitosis to be called when analysis recommends split"
fi

test_case "_needle_check_mitosis returns 0 when mitosis is performed"
MOCK_PERFORM_RC=0
MOCK_ANALYSIS_JSON='{"mitosis":true,"reasoning":"split","children":[{"title":"C1","description":"d1","affected_files":[],"verification_cmd":"","labels":[],"blocked_by":[]},{"title":"C2","description":"d2","affected_files":[],"verification_cmd":"","labels":[],"blocked_by":[]}]}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
rc=$?
if [[ $rc -eq 0 ]]; then
    test_pass
else
    test_fail "Expected exit code 0 when mitosis succeeds, got $rc"
fi

test_case "_needle_check_mitosis returns 1 when analysis says no split"
MOCK_ANALYSIS_JSON='{"mitosis":false,"reasoning":"atomic task","children":[]}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
rc=$?
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit when analysis recommends no split"
fi

test_case "_needle_check_mitosis does not call perform when analysis says no split"
MOCK_ANALYSIS_JSON='{"mitosis":false,"reasoning":"atomic","children":[]}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
if [[ ! -s "$PERFORM_LOG" ]]; then
    test_pass
else
    test_fail "Expected _needle_perform_mitosis NOT to be called when analysis says no split"
fi

test_case "_needle_check_mitosis returns 1 when analysis returns empty"
_needle_analyze_for_mitosis() {
    echo "$*" >> "$ANALYZE_LOG"
    echo ""
    return 0
}
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
rc=$?
# Restore mock
_needle_analyze_for_mitosis() {
    echo "$*" >> "$ANALYZE_LOG"
    echo "$MOCK_ANALYSIS_JSON"
    return 0
}
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit when analysis returns empty"
fi

# ============================================================================
# Array bead response handling
# ============================================================================

test_case "_needle_check_mitosis handles array response from br show"
# br returns a JSON array (as br sometimes does)
br() {
    echo "$*" >> "$BR_LOG"
    case "$1" in
        show) echo '[{"id":"nd-test","issue_type":"task","labels":[],"description":"line1\nline2\nline3\nline4\nline5","priority":2}]' ;;
    esac
    return 0
}
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" &>/dev/null
result=$?
# Restore br mock
br() {
    echo "$*" >> "$BR_LOG"
    case "$1" in
        show) echo "$MOCK_BEAD_JSON" ;;
    esac
    return 0
}
# Analysis should be called (type/label checks passed from array response)
if grep -q "nd-test" "$ANALYZE_LOG" 2>/dev/null; then
    test_pass
else
    test_fail "Expected analysis to proceed when br returns array response"
fi

# ============================================================================
# Force parameter — bypasses min_complexity
# ============================================================================

test_case "_needle_check_mitosis with force=true bypasses min_complexity for short description"
# 2 lines < min_complexity=3, but force=true should bypass the check
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"task","labels":[],"description":"line1\nline2","priority":2}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" "true" "3" &>/dev/null
if grep -q "nd-test" "$ANALYZE_LOG" 2>/dev/null; then
    test_pass
else
    test_fail "Expected analysis to be called when force=true despite short description"
fi

test_case "_needle_check_mitosis with force=false still applies min_complexity gate"
# 2 lines < min_complexity=3 and force=false → should return 1
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"task","labels":[],"description":"line1\nline2","priority":2}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" "false" "0" &>/dev/null
rc=$?
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit for short description when force=false"
fi

test_case "_needle_check_mitosis with force=true still skips skip_labels"
# Even with force=true, skip labels are respected
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"task","labels":["no-mitosis"],"description":"line1\nline2","priority":2}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" "true" "3" &>/dev/null
rc=$?
if [[ $rc -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit for no-mitosis label even with force=true"
fi

test_case "_needle_check_mitosis passes failure_count through to analyze"
MOCK_BEAD_JSON='{"id":"nd-test","issue_type":"task","labels":[],"description":"line1\nline2","priority":2}'
_needle_check_mitosis "nd-test" "$WS_DIR" "agent" "true" "5" &>/dev/null
if grep -q "5" "$ANALYZE_LOG" 2>/dev/null; then
    test_pass
else
    test_fail "Expected failure_count=5 to appear in analyze call args"
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
