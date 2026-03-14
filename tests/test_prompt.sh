#!/usr/bin/env bash
# Tests for NEEDLE prompt builder module (src/bead/prompt.sh)

# Test setup - create temp directory
TEST_DIR=$(mktemp -d)
TEST_WORKSPACE="$TEST_DIR/workspace"
mkdir -p "$TEST_WORKSPACE"

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_HOME="$TEST_DIR/.needle"
export NEEDLE_STATE_DIR="state"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=false

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/bead/prompt.sh"

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

# Mock br show command for testing
mock_br_show() {
    local data="$1"
    # Create a mock br script
    mkdir -p "$TEST_DIR/bin"
    cat > "$TEST_DIR/bin/br" << EOF
#!/bin/bash
case "\$1 \$2 \$3" in
    "show "*" --json")
        echo '$data'
        ;;
    *)
        echo "[]" >&2
        exit 1
        ;;
esac
EOF
    chmod +x "$TEST_DIR/bin/br"
    export PATH="$TEST_DIR/bin:$PATH"
}

# Remove mock
unmock_br() {
    export PATH="${PATH//$TEST_DIR\/bin:/}"
}

echo "=== NEEDLE Prompt Builder Tests ==="
echo ""

# Test 3: Shell escaping
test_case "Escapes single quotes in prompt"
original="It's a test"
escaped=$(_needle_escape_prompt "$original")

# Verify the escaped string can be safely embedded
eval "result='$escaped'"
if [[ "$result" == "$original" ]]; then
    test_pass
else
    test_fail "Escaping broke the string: got '$result'"
fi

test_case "Escapes multiple single quotes"
original="It's a 'test' with quotes"
escaped=$(_needle_escape_prompt "$original")

eval "result='$escaped'"
if [[ "$result" == "$original" ]]; then
    test_pass
else
    test_fail "Escaping broke the string: got '$result'"
fi

test_case "Handles newlines in prompt"
original=$'Line 1\nLine 2\nLine 3'
escaped=$(_needle_escape_prompt "$original")

eval "result='$escaped'"
if [[ "$result" == "$original" ]]; then
    test_pass
else
    test_fail "Escaping broke newlines"
fi

# Test 4: JSON escaping
test_case "Escapes prompt for JSON"
original='Test with "quotes" and \backslashes\'
escaped=$(_needle_escape_json "$original")

if [[ "$escaped" == *'"Test with '* ]] && [[ "$escaped" == *'\\'* ]]; then
    test_pass
else
    test_fail "JSON escaping failed: $escaped"
fi

# Test 5: Prompt validation
test_case "Validates correct prompt structure"
prompt="# Task: Test
## Bead ID
nd-test
## Description
A test task
## Workspace
/test"

if _needle_validate_prompt "$prompt" &>/dev/null; then
    test_pass
else
    test_fail "Valid prompt rejected"
fi

test_case "Rejects empty prompt"
if ! _needle_validate_prompt "" &>/dev/null; then
    test_pass
else
    test_fail "Empty prompt should be rejected"
fi

test_case "Rejects prompt without Bead ID"
prompt="# Task: Test
## Description
A test task"

if ! _needle_validate_prompt "$prompt" &>/dev/null; then
    test_pass
else
    test_fail "Prompt without Bead ID should be rejected"
fi

test_case "Rejects prompt without Description"
prompt="# Task: Test
## Bead ID
nd-test"

if ! _needle_validate_prompt "$prompt" &>/dev/null; then
    test_pass
else
    test_fail "Prompt without Description should be rejected"
fi

# Test 6: Full prompt building
test_case "Builds prompt from bead JSON"
mock_br_show '[{
    "id": "nd-test1",
    "title": "Test Task",
    "description": "A test task description",
    "labels": ["test", "unit"],
    "status": "open",
    "priority": 1
}]'

prompt=$(_needle_build_prompt "nd-test1" "$TEST_WORKSPACE" 2>/dev/null)

if [[ "$prompt" == *"Test Task"* ]] && \
   [[ "$prompt" == *"nd-test1"* ]] && \
   [[ "$prompt" == *"A test task description"* ]] && \
   [[ "$prompt" == *"test, unit"* ]] && \
   [[ "$prompt" == *"P1 (high)"* ]]; then
    test_pass
else
    test_fail "Prompt missing expected content"
fi

test_case "Handles missing labels"
mock_br_show '[{
    "id": "nd-test2",
    "title": "Task Without Labels",
    "description": "No labels here",
    "status": "open",
    "priority": 2
}]'

prompt=$(_needle_build_prompt "nd-test2" "$TEST_WORKSPACE" 2>/dev/null)

if [[ "$prompt" == *"Task Without Labels"* ]] && [[ "$prompt" == *"nd-test2"* ]]; then
    test_pass
else
    test_fail "Failed to build prompt without labels"
fi

test_case "Handles empty description"
mock_br_show '[{
    "id": "nd-test3",
    "title": "Task Without Description",
    "description": "",
    "status": "open",
    "priority": 2
}]'

prompt=$(_needle_build_prompt "nd-test3" "$TEST_WORKSPACE" 2>/dev/null)

if [[ "$prompt" == *"Task Without Description"* ]]; then
    test_pass
else
    test_fail "Failed to build prompt with empty description"
fi

test_case "Includes Tier 3 context for standalone bead (no genesis)"
mock_br_show '[{
    "id": "nd-test4",
    "title": "Task With No Genesis",
    "description": "A standalone task",
    "status": "open",
    "priority": 2
}]'

prompt=$(_needle_build_prompt "nd-test4" "$TEST_WORKSPACE" 2>/dev/null)

if [[ "$prompt" == *"standalone task with no linked project plan"* ]]; then
    test_pass
else
    test_fail "Expected Tier 3 standalone context"
fi

# Test three-tier genesis context
test_case "Tier 1: Genesis with plan path included in prompt"
# Mock: bead depends on genesis bead that has a plan path
mkdir -p "$TEST_DIR/bin2"
cat > "$TEST_DIR/bin2/br" << 'MOCKEOF'
#!/bin/bash
case "$1 $2 $3" in
    "show nd-child --json")
        echo '[{"id":"nd-child","title":"Child Task","description":"A child task","labels":[],"status":"open","priority":2,"issue_type":"task","dependencies":[{"id":"nd-genesis","rel":"blocked_by"}]}]'
        ;;
    "show nd-genesis --json")
        echo '[{"id":"nd-genesis","title":"Genesis: MyProject","description":"## Genesis Bead\nTied to plan: /home/coding/myproject/docs/plan.md","labels":[],"status":"open","priority":1,"issue_type":"genesis","dependencies":[]}]'
        ;;
    *)
        echo "[]" >&2
        exit 1
        ;;
esac
MOCKEOF
chmod +x "$TEST_DIR/bin2/br"
OLD_PATH="$PATH"
export PATH="$TEST_DIR/bin2:$PATH"

prompt=$(_needle_build_prompt "nd-child" "$TEST_WORKSPACE" 2>/dev/null)

if [[ "$prompt" == *"Genesis: MyProject"* ]] && \
   [[ "$prompt" == *"/home/coding/myproject/docs/plan.md"* ]] && \
   [[ "$prompt" == *"Review the plan"* ]]; then
    test_pass
else
    test_fail "Expected Tier 1 genesis+plan context"
fi

test_case "Tier 2: Genesis without plan path prompts discovery"
cat > "$TEST_DIR/bin2/br" << 'MOCKEOF'
#!/bin/bash
case "$1 $2 $3" in
    "show nd-child2 --json")
        echo '[{"id":"nd-child2","title":"Child Task 2","description":"A child task","labels":[],"status":"open","priority":2,"issue_type":"task","dependencies":[{"id":"nd-genesis2","rel":"blocked_by"}]}]'
        ;;
    "show nd-genesis2 --json")
        echo '[{"id":"nd-genesis2","title":"Genesis: AnotherProject","description":"## Genesis Bead\nOverview of the project.","labels":[],"status":"open","priority":1,"issue_type":"genesis","dependencies":[]}]'
        ;;
    *)
        echo "[]" >&2
        exit 1
        ;;
esac
MOCKEOF

prompt=$(_needle_build_prompt "nd-child2" "$TEST_WORKSPACE" 2>/dev/null)

if [[ "$prompt" == *"Genesis: AnotherProject"* ]] && \
   [[ "$prompt" == *"does not reference a plan document directly"* ]] && \
   [[ "$prompt" == *"br list --status closed --json"* ]]; then
    test_pass
else
    test_fail "Expected Tier 2 genesis-no-plan discovery instructions"
fi

export PATH="$OLD_PATH"

# Test 7: Error handling
test_case "Returns error for missing bead ID"
if ! _needle_build_prompt "" "$TEST_WORKSPACE" &>/dev/null; then
    test_pass
else
    test_fail "Should fail with missing bead ID"
fi

test_case "Returns error for missing workspace"
if ! _needle_build_prompt "nd-test" "" &>/dev/null; then
    test_pass
else
    test_fail "Should fail with missing workspace"
fi

test_case "Returns error for non-existent workspace"
if ! _needle_build_prompt "nd-test" "/nonexistent/path" &>/dev/null; then
    test_pass
else
    test_fail "Should fail with non-existent workspace"
fi

test_case "Returns error for non-existent bead"
mock_br_show '[]'

if ! _needle_build_prompt "nd-nonexistent" "$TEST_WORKSPACE" &>/dev/null; then
    test_pass
else
    test_fail "Should fail for non-existent bead"
fi

# Test 8: Minimal prompt
test_case "Builds minimal prompt"
mock_br_show '[{
    "id": "nd-minimal",
    "title": "Minimal Task",
    "description": "A minimal task description that might be quite long but should be truncated in the minimal view"
}]'

prompt=$(_needle_build_minimal_prompt "nd-minimal" "$TEST_WORKSPACE" 2>/dev/null)

if [[ "$prompt" == *"Minimal Task"* ]] && [[ "$prompt" == *"nd-minimal"* ]]; then
    test_pass
else
    test_fail "Minimal prompt missing expected content"
fi

# Cleanup
unmock_br

# Print summary
echo ""
echo "=== Test Summary ==="
echo "Tests run: $TESTS_RUN"
echo "Passed: $TESTS_PASSED"
echo "Failed: $TESTS_FAILED"

if [[ $TESTS_FAILED -eq 0 ]]; then
    echo ""
    echo "All tests passed!"
    exit 0
else
    echo ""
    echo "Some tests failed!"
    exit 1
fi
