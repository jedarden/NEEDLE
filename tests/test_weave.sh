#!/usr/bin/env bash
# Test script for strands/weave.sh module
#
# Validates:
# - Doc gap analysis implementation
# - Deduplication against existing open beads
# - Opt-in configuration setting (strands.weave: false by default)
# - Prompt template matches plan.md
# - Frequency limiting
# - JSON parsing
# - Bead creation with type field
# - Statistics functions

# Don't use set -e because arithmetic ((++)) can return 1 and trigger exit

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Set up test environment BEFORE sourcing any modules
NEEDLE_HOME="$HOME/.needle-test-weave-$$"
NEEDLE_CONFIG_NAME="config.yaml"
NEEDLE_CONFIG_FILE="$NEEDLE_HOME/$NEEDLE_CONFIG_NAME"
NEEDLE_SESSION="test-weave-$$"
NEEDLE_WORKSPACE="/tmp/test-workspace-weave-$$"
NEEDLE_AGENT="test-agent"
NEEDLE_VERBOSE=true
NEEDLE_STATE_DIR="state"
NEEDLE_LOG_DIR="logs"
NEEDLE_LOG_FILE="$NEEDLE_HOME/$NEEDLE_LOG_DIR/$(date +%Y-%m-%d).jsonl"

# Create test directories
mkdir -p "$NEEDLE_HOME/$NEEDLE_STATE_DIR"
mkdir -p "$NEEDLE_HOME/$NEEDLE_LOG_DIR"
mkdir -p "$NEEDLE_WORKSPACE"

# Create a minimal config file with weave enabled for most tests
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
strands:
  pluck: true
  explore: true
  mend: true
  weave: true
  unravel: false
  pulse: false
  knot: true

strands.weave.frequency: 3600
strands.weave.max_doc_files: 50
strands.weave.max_beads_per_run: 5
EOF

# Source required libraries AFTER setting up environment
source "$PROJECT_ROOT/src/lib/constants.sh"
source "$PROJECT_ROOT/src/lib/output.sh"
source "$PROJECT_ROOT/src/lib/paths.sh"
source "$PROJECT_ROOT/src/lib/json.sh"
source "$PROJECT_ROOT/src/lib/utils.sh"
source "$PROJECT_ROOT/src/lib/config.sh"

# Source the weave module
source "$PROJECT_ROOT/src/strands/weave.sh"

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

# Mock br command for testing
br() {
    case "$1" in
        list)
            echo '[]'
            ;;
        create)
            echo "nd-weave-test-$$"
            return 0
            ;;
        *)
            return 0
            ;;
    esac
}

# Mock _needle_dispatch_agent for testing
_needle_dispatch_agent() {
    local output_file
    output_file=$(mktemp)

    cat > "$output_file" << 'EOF'
```json
{
  "gaps": [
    {
      "title": "Implement feature X from roadmap",
      "description": "The roadmap mentions feature X but it is not yet implemented",
      "source_file": "docs/ROADMAP.md",
      "source_line": "Feature X: Add support for Y",
      "priority": 2,
      "type": "feature",
      "estimated_effort": "medium"
    }
  ]
}
```
EOF

    echo "0|5000|$output_file"
}

# Mock _needle_create_bead for testing
_needle_create_bead() {
    echo "nd-weave-created-$$"
    return 0
}

# Cleanup function
cleanup() {
    rm -rf "$NEEDLE_HOME"
    rm -rf "$NEEDLE_WORKSPACE"
}
trap cleanup EXIT

# Run tests
echo "=========================================="
echo "Running strands/weave.sh tests"
echo "=========================================="

# ============================================================================
# Test 1: Opt-in check returns true when enabled
# ============================================================================
_test_start "Opt-in check returns true when strands.weave is true"
if _needle_weave_is_enabled; then
    _test_pass "Opt-in check returns true when enabled"
else
    _test_fail "Opt-in check returned false when should be true"
fi

# ============================================================================
# Test 2: Opt-in check returns false by default (false config)
# ============================================================================
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
strands:
  weave: false
EOF
clear_config_cache
_test_start "Opt-in check returns false when strands.weave is false"
if ! _needle_weave_is_enabled; then
    _test_pass "Opt-in check correctly returns false when disabled"
else
    _test_fail "Opt-in check should return false when strands.weave is false"
fi

# ============================================================================
# Test 3: Opt-in check returns false when not configured (default)
# ============================================================================
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
strands:
  pluck: true
EOF
clear_config_cache
_test_start "Opt-in check returns false when not configured (false by default)"
if ! _needle_weave_is_enabled; then
    _test_pass "Opt-in check correctly defaults to false (opt-in required)"
else
    _test_fail "Opt-in check should default to false - weave is opt-in only"
fi

# Restore enabled config for remaining tests
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
strands:
  weave: true

strands.weave.frequency: 3600
strands.weave.max_doc_files: 50
strands.weave.max_beads_per_run: 5
EOF
clear_config_cache

# ============================================================================
# Test 4: Disabled strand returns failure without running
# ============================================================================
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
strands:
  weave: false
EOF
clear_config_cache
_test_start "Strand returns failure when disabled"
if ! _needle_strand_weave "$NEEDLE_WORKSPACE" "test-agent"; then
    _test_pass "Disabled strand correctly returns failure"
else
    _test_fail "Disabled strand should return failure"
fi

# Restore enabled config
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
strands:
  weave: true

strands.weave.frequency: 3600
strands.weave.max_doc_files: 50
strands.weave.max_beads_per_run: 5
EOF
clear_config_cache

# ============================================================================
# Test 5: Find docs finds markdown files
# ============================================================================
_test_start "Find docs discovers markdown files"
mkdir -p "$NEEDLE_WORKSPACE/docs"
echo "# Test Roadmap" > "$NEEDLE_WORKSPACE/ROADMAP.md"
echo "# Test README" > "$NEEDLE_WORKSPACE/README.md"
echo "# Docs" > "$NEEDLE_WORKSPACE/docs/guide.md"

docs=$(_needle_weave_find_docs "$NEEDLE_WORKSPACE")
if echo "$docs" | grep -q "ROADMAP.md" && echo "$docs" | grep -q "README.md"; then
    _test_pass "Find docs discovers ROADMAP.md and README.md"
else
    _test_fail "Find docs missing expected files. Got: $docs"
fi

# ============================================================================
# Test 6: Find docs excludes .beads directory
# ============================================================================
_test_start "Find docs excludes .beads directory"
mkdir -p "$NEEDLE_WORKSPACE/.beads"
echo "# Bead doc" > "$NEEDLE_WORKSPACE/.beads/internal.md"

docs=$(_needle_weave_find_docs "$NEEDLE_WORKSPACE")
if ! echo "$docs" | grep -q "\.beads"; then
    _test_pass "Find docs correctly excludes .beads directory"
else
    _test_fail "Find docs should exclude .beads directory"
fi

# ============================================================================
# Test 7: Prompt template matches plan.md structure
# ============================================================================
_test_start "Prompt template matches plan.md structure"
test_docs="$NEEDLE_WORKSPACE/README.md"
test_beads='["Existing bead 1"]'
prompt=$(_needle_weave_build_prompt "$NEEDLE_WORKSPACE" "$test_docs" "$test_beads")

if echo "$prompt" | grep -q "analyzing a codebase for gaps between documentation and implementation" && \
   echo "$prompt" | grep -q "NOT already tracked as open beads" && \
   echo "$prompt" | grep -q "NOT already implemented in the codebase" && \
   echo "$prompt" | grep -q "source_line" && \
   echo "$prompt" | grep -q '"type": "task|bug|feature"' && \
   echo "$prompt" | grep -q '"estimated_effort": "small|medium|large"'; then
    _test_pass "Prompt template matches plan.md structure"
else
    _test_fail "Prompt template missing required plan.md fields"
    # Debug output
    echo "    Prompt content:"
    echo "$prompt" | head -20
fi

# ============================================================================
# Test 8: Prompt template includes open beads for deduplication
# ============================================================================
_test_start "Prompt includes open beads for deduplication"
if echo "$prompt" | grep -q "Current Open Beads" && \
   echo "$prompt" | grep -q "Existing bead 1"; then
    _test_pass "Prompt includes open beads section"
else
    _test_fail "Prompt missing open beads section"
fi

# ============================================================================
# Test 9: Prompt instructs to output empty gaps when none found
# ============================================================================
_test_start "Prompt includes instruction for empty gaps output"
if echo "$prompt" | grep -q '{"gaps": \[\]}'; then
    _test_pass "Prompt includes empty gaps instruction from plan.md"
else
    _test_fail "Prompt missing empty gaps instruction"
fi

# ============================================================================
# Test 10: Parse gaps extracts JSON from code block
# ============================================================================
_test_start "Parse gaps extracts JSON from markdown code block"
test_output='```json
{
  "gaps": [
    {
      "title": "Test Gap",
      "description": "A test gap",
      "source_file": "README.md",
      "source_line": "TODO: implement this",
      "priority": 2,
      "type": "task",
      "estimated_effort": "small"
    }
  ]
}
```'
gaps=$(_needle_weave_parse_gaps "$test_output")
if echo "$gaps" | grep -q "Test Gap"; then
    _test_pass "Parse gaps extracts JSON correctly"
else
    _test_fail "Parse gaps failed to extract. Got: $gaps"
fi

# ============================================================================
# Test 11: Parse gaps returns empty array for no gaps
# ============================================================================
_test_start "Parse gaps returns empty array when no gaps found"
gaps=$(_needle_weave_parse_gaps '{"gaps": []}')
if [[ "$gaps" == "[]" ]]; then
    _test_pass "Parse gaps returns empty array for no gaps"
else
    _test_fail "Parse gaps should return [], got: $gaps"
fi

# ============================================================================
# Test 12: Parse gaps handles invalid input
# ============================================================================
_test_start "Parse gaps handles invalid/missing JSON gracefully"
gaps=$(_needle_weave_parse_gaps "No JSON here at all")
if [[ "$gaps" == "[]" ]]; then
    _test_pass "Parse gaps returns empty array for invalid input"
else
    _test_fail "Parse gaps should return [] for invalid input, got: $gaps"
fi

# ============================================================================
# Test 13: Create beads uses type field from gap JSON
# ============================================================================
_test_start "Create beads uses type field (task|bug|feature)"
CAPTURE_FILE=$(mktemp)
_needle_create_bead() {
    echo "$@" > "$CAPTURE_FILE"
    echo "nd-weave-type-$$"
    return 0
}
test_gaps='[{"title": "Feature gap", "description": "A feature", "priority": 2, "type": "feature", "source_file": "docs/plan.md", "source_line": "Feature: XYZ"}]'
_needle_weave_create_beads "$NEEDLE_WORKSPACE" "$test_gaps" >/dev/null 2>&1
if grep -q "\-\-type feature" "$CAPTURE_FILE"; then
    _test_pass "Create beads passes type=feature to bead creation"
else
    _test_fail "Create beads missing type field. Got: $(cat "$CAPTURE_FILE")"
fi
rm -f "$CAPTURE_FILE"

# ============================================================================
# Test 14: Create beads defaults type to task when not specified
# ============================================================================
_test_start "Create beads defaults to type=task when type not in gap"
CAPTURE_FILE=$(mktemp)
_needle_create_bead() {
    echo "$@" > "$CAPTURE_FILE"
    echo "nd-weave-default-$$"
    return 0
}
test_gaps='[{"title": "Untyped gap", "description": "No type field", "priority": 2}]'
_needle_weave_create_beads "$NEEDLE_WORKSPACE" "$test_gaps" >/dev/null 2>&1
if grep -q "\-\-type task" "$CAPTURE_FILE"; then
    _test_pass "Create beads defaults to type=task"
else
    _test_fail "Create beads should default to type=task. Got: $(cat "$CAPTURE_FILE")"
fi
rm -f "$CAPTURE_FILE"

# ============================================================================
# Test 15: Create beads adds weave-generated and from-docs labels
# ============================================================================
_test_start "Create beads adds weave-generated and from-docs labels"
CAPTURE_FILE=$(mktemp)
_needle_create_bead() {
    echo "$@" > "$CAPTURE_FILE"
    echo "nd-weave-labels-$$"
    return 0
}
test_gaps='[{"title": "Labeled gap", "description": "Test", "priority": 2, "type": "task"}]'
_needle_weave_create_beads "$NEEDLE_WORKSPACE" "$test_gaps" >/dev/null 2>&1
if grep -q "weave-generated" "$CAPTURE_FILE" && grep -q "from-docs" "$CAPTURE_FILE"; then
    _test_pass "Create beads adds weave-generated and from-docs labels"
else
    _test_fail "Create beads missing required labels. Got: $(cat "$CAPTURE_FILE")"
fi
rm -f "$CAPTURE_FILE"

# ============================================================================
# Test 16: Create beads includes source_line in description
# ============================================================================
_test_start "Create beads includes source_line in bead description"
CAPTURE_FILE=$(mktemp)
_needle_create_bead() {
    echo "$@" > "$CAPTURE_FILE"
    echo "nd-weave-source-$$"
    return 0
}
test_gaps='[{"title": "Gap with source", "description": "Test desc", "priority": 2, "type": "task", "source_file": "ROADMAP.md", "source_line": "Feature: Add X"}]'
_needle_weave_create_beads "$NEEDLE_WORKSPACE" "$test_gaps" >/dev/null 2>&1
if grep -q "Feature: Add X" "$CAPTURE_FILE" || grep -q "ROADMAP.md" "$CAPTURE_FILE"; then
    _test_pass "Create beads includes source context in description"
else
    _test_fail "Create beads missing source context. Got: $(cat "$CAPTURE_FILE")"
fi
rm -f "$CAPTURE_FILE"

# Restore _needle_create_bead mock
_needle_create_bead() {
    echo "nd-weave-test-$$"
    return 0
}

# ============================================================================
# Test 17: Frequency check prevents rapid re-runs
# ============================================================================
_test_start "Frequency check prevents rapid re-runs"
# Record a run now
_needle_weave_record_run "$NEEDLE_WORKSPACE"
# Now check - should be rate limited
if ! _needle_weave_check_frequency "$NEEDLE_WORKSPACE"; then
    _test_pass "Frequency check correctly rate-limits after recent run"
else
    _test_fail "Frequency check should rate-limit after recent run"
fi

# ============================================================================
# Test 18: Clear rate limit allows re-run
# ============================================================================
_test_start "Clear rate limit allows re-run"
_needle_weave_clear_rate_limit "$NEEDLE_WORKSPACE"
if _needle_weave_check_frequency "$NEEDLE_WORKSPACE"; then
    _test_pass "After clearing rate limit, frequency check passes"
else
    _test_fail "After clearing rate limit, frequency check should pass"
fi

# ============================================================================
# Test 19: Get open beads returns empty array when no beads exist
# ============================================================================
_test_start "Get open beads returns empty array when no beads"
br() {
    case "$1" in
        list) echo '[]' ;;
        *) return 0 ;;
    esac
}
open_beads=$(_needle_weave_get_open_beads "$NEEDLE_WORKSPACE")
if [[ "$open_beads" == "[]" ]]; then
    _test_pass "Get open beads returns [] when no beads exist"
else
    _test_fail "Get open beads should return [], got: $open_beads"
fi

# ============================================================================
# Test 20: Get open beads extracts titles for deduplication
# ============================================================================
_test_start "Get open beads extracts titles for deduplication"
br() {
    case "$1" in
        list)
            echo '[{"id":"nd-1","title":"Fix authentication bug","status":"open"},{"id":"nd-2","title":"Add logging","status":"open"}]'
            ;;
        *) return 0 ;;
    esac
}
open_beads=$(_needle_weave_get_open_beads "$NEEDLE_WORKSPACE")
if echo "$open_beads" | grep -q "Fix authentication bug" && echo "$open_beads" | grep -q "Add logging"; then
    _test_pass "Get open beads extracts titles for deduplication"
else
    _test_fail "Get open beads should include bead titles. Got: $open_beads"
fi

# ============================================================================
# Test 21: Stats function returns valid JSON
# ============================================================================
_test_start "Stats function returns valid JSON"
stats=$(_needle_weave_stats)
if echo "$stats" | jq -e . >/dev/null 2>&1; then
    _test_pass "Stats function returns valid JSON"
else
    _test_fail "Stats function returned invalid JSON: $stats"
fi

# ============================================================================
# Test 22: Create beads respects max_beads_per_run limit
# ============================================================================
_test_start "Create beads respects max_beads_per_run limit"
# The config file uses flat dot-notation (strands.weave.max_beads_per_run: 5) which
# get_config cannot resolve as a nested path. Invoke the function directly with a
# max_beads env var by temporarily wrapping _needle_weave_create_beads.
test_gaps='[
  {"title":"Gap 1","description":"D1","priority":2,"type":"task"},
  {"title":"Gap 2","description":"D2","priority":2,"type":"task"},
  {"title":"Gap 3","description":"D3","priority":2,"type":"task"},
  {"title":"Gap 4","description":"D4","priority":2,"type":"task"},
  {"title":"Gap 5","description":"D5","priority":2,"type":"task"},
  {"title":"Gap 6","description":"D6","priority":2,"type":"task"},
  {"title":"Gap 7","description":"D7","priority":2,"type":"task"}
]'
_needle_create_bead() {
    echo "nd-weave-limit-$$"
    return 0
}
# Patch get_config only for this test to return the configured max
# (config file uses flat dot-notation that get_config can't resolve as nested path)
get_config() {
    local key="$1"
    local default="${2:-}"
    if [[ "$key" == "strands.weave.max_beads_per_run" ]]; then
        echo "5"; return 0
    fi
    local value
    value=$(load_config | jq -r ".$key" 2>/dev/null)
    if [[ "$value" == "null" ]] || [[ -z "$value" ]]; then
        echo "$default"
    else
        echo "$value"
    fi
}
created=$(_needle_weave_create_beads "$NEEDLE_WORKSPACE" "$test_gaps" 2>/dev/null)
# Unset patched get_config to restore the sourced version
unset -f get_config
source "$PROJECT_ROOT/src/lib/config.sh"
if [[ "$created" =~ ^[0-9]+$ ]] && [[ "$created" -le 5 ]]; then
    _test_pass "Create beads respects max_beads_per_run limit (created $created)"
else
    _test_fail "Create beads exceeded max_beads_per_run. Created: $created"
fi

# ============================================================================
# Test 23: Create beads adds verification_cmd as label when present
# ============================================================================
_test_start "Create beads adds verification_cmd as label when present"
CAPTURE_FILE=$(mktemp)
_needle_create_bead() {
    echo "$@" > "$CAPTURE_FILE"
    echo "nd-weave-vcmd-$$"
    return 0
}
test_gaps='[{"title": "Verified gap", "description": "Test", "priority": 2, "type": "task", "verification_cmd": "pytest tests/test_foo.py -q"}]'
_needle_weave_create_beads "$NEEDLE_WORKSPACE" "$test_gaps" >/dev/null 2>&1
if grep -q "verification_cmd:pytest tests/test_foo.py -q" "$CAPTURE_FILE"; then
    _test_pass "Create beads adds verification_cmd label in format verification_cmd:<cmd>"
else
    _test_fail "Create beads missing verification_cmd label. Got: $(cat "$CAPTURE_FILE")"
fi
rm -f "$CAPTURE_FILE"

# ============================================================================
# Test 24: Create beads omits verification_cmd label when absent
# ============================================================================
_test_start "Create beads omits verification_cmd label when not in gap"
CAPTURE_FILE=$(mktemp)
_needle_create_bead() {
    echo "$@" > "$CAPTURE_FILE"
    echo "nd-weave-novcmd-$$"
    return 0
}
test_gaps='[{"title": "Unverified gap", "description": "No verification", "priority": 2, "type": "task"}]'
_needle_weave_create_beads "$NEEDLE_WORKSPACE" "$test_gaps" >/dev/null 2>&1
if ! grep -q "verification_cmd:" "$CAPTURE_FILE"; then
    _test_pass "Create beads correctly omits verification_cmd label when absent"
else
    _test_fail "Create beads should not include verification_cmd label. Got: $(cat "$CAPTURE_FILE")"
fi
rm -f "$CAPTURE_FILE"

# ============================================================================
# Test 25: Prompt template includes verification_cmd field documentation
# ============================================================================
_test_start "Prompt template includes verification_cmd field documentation"
# Reset _needle_create_bead to default mock
_needle_create_bead() {
    echo "nd-weave-test-$$"
    return 0
}
prompt=$(_needle_weave_build_prompt "$NEEDLE_WORKSPACE" "$NEEDLE_WORKSPACE/README.md" '[]' 2>/dev/null)
if echo "$prompt" | grep -q "verification_cmd"; then
    _test_pass "Prompt template includes verification_cmd in output schema"
else
    _test_fail "Prompt template missing verification_cmd field documentation"
fi

# Summary
echo ""
echo "=========================================="
echo "Test Summary"
echo "=========================================="
echo "Passed: $TESTS_PASSED"
echo "Failed: $TESTS_FAILED"
echo ""

if [[ $TESTS_FAILED -eq 0 ]]; then
    echo "All tests passed!"
    exit 0
else
    echo "Some tests failed!"
    exit 1
fi
