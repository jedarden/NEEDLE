#!/usr/bin/env bash
# Test script for strands/unravel.sh module

# Don't use set -e because arithmetic ((++)) can return 1 and trigger exit

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Set up test environment BEFORE sourcing any modules
NEEDLE_HOME="$HOME/.needle-test-unravel-$$"
NEEDLE_CONFIG_NAME="config.yaml"
NEEDLE_CONFIG_FILE="$NEEDLE_HOME/$NEEDLE_CONFIG_NAME"
NEEDLE_SESSION="test-unravel-$$"
NEEDLE_WORKSPACE="/tmp/test-workspace-unravel"
NEEDLE_AGENT="test-agent"
NEEDLE_VERBOSE=true
NEEDLE_STATE_DIR="state"
NEEDLE_LOG_DIR="logs"
NEEDLE_LOG_FILE="$NEEDLE_HOME/$NEEDLE_LOG_DIR/$(date +%Y-%m-%d).jsonl"

# Create test directories
mkdir -p "$NEEDLE_HOME/$NEEDLE_STATE_DIR"
mkdir -p "$NEEDLE_HOME/$NEEDLE_LOG_DIR"
mkdir -p "$NEEDLE_WORKSPACE"

# Create a minimal config file for testing with unravel enabled
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
strands:
  pluck: true
  explore: true
  mend: true
  weave: false
  unravel: true
  pulse: false
  knot: true

unravel:
  min_wait_hours: 24
  max_alternatives: 3
  timeout: 120
EOF

# Source required libraries AFTER setting up environment
source "$PROJECT_ROOT/src/lib/constants.sh"
source "$PROJECT_ROOT/src/lib/output.sh"
source "$PROJECT_ROOT/src/lib/paths.sh"
source "$PROJECT_ROOT/src/lib/json.sh"
source "$PROJECT_ROOT/src/lib/utils.sh"
source "$PROJECT_ROOT/src/lib/config.sh"

# Source the unravel module
source "$PROJECT_ROOT/src/strands/unravel.sh"

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
            # Handle different list options
            if [[ "$*" == *"--status blocked"* ]] && [[ "$*" == *"--type human"* ]]; then
                # Return mock blocked HUMAN beads
                local now=$(date +%s)
                local old_ts=$((now - 86400))  # 24 hours ago
                local recent_ts=$((now - 3600))  # 1 hour ago

                cat << EOF
[
  {
    "id": "nd-human-old",
    "title": "Old HUMAN bead waiting for input",
    "description": "This bead has been waiting for 24+ hours",
    "status": "blocked",
    "issue_type": "human",
    "created_at": "$old_ts"
  },
  {
    "id": "nd-human-recent",
    "title": "Recent HUMAN bead",
    "description": "This bead has only been waiting for 1 hour",
    "status": "blocked",
    "issue_type": "human",
    "created_at": "$recent_ts"
  }
]
EOF
            elif [[ "$*" == *"--label"* ]]; then
                # Extract label from command
                if [[ "$*" == *"for-nd-human-old"* ]]; then
                    # Simulate no existing alternatives
                    echo '[]'
                else
                    echo '[]'
                fi
            else
                echo '[]'
            fi
            ;;
        show)
            local bead_id="$2"
            if [[ "$bead_id" == "nd-human-old" ]]; then
                local now=$(date +%s)
                local old_ts=$((now - 86400))
                cat << EOF
{
  "id": "nd-human-old",
  "title": "Old HUMAN bead waiting for input",
  "description": "This bead has been waiting for 24+ hours",
  "status": "blocked",
  "issue_type": "human",
  "created_at": "$old_ts"
}
EOF
            else
                echo 'null'
            fi
            ;;
        create)
            # Return a mock bead ID
            echo "nd-unravel-test-$$"
            return 0
            ;;
        *)
            return 0
            ;;
    esac
}

# Mock _needle_dispatch_agent for testing
_needle_dispatch_agent() {
    # Return a mock analysis result
    local output_file
    output_file=$(mktemp)

    cat > "$output_file" << 'EOF'
```json
{
  "alternatives": [
    {
      "title": "Alternative approach 1",
      "description": "Description for alternative 1",
      "approach": "Step 1: Do this. Step 2: Do that.",
      "reversible": true,
      "risks": ["Risk 1", "Risk 2"],
      "benefits": ["Benefit 1", "Benefit 2"]
    },
    {
      "title": "Alternative approach 2",
      "description": "Description for alternative 2",
      "approach": "Alternative implementation path",
      "reversible": true,
      "risks": ["Minor risk"],
      "benefits": ["Quick win"]
    }
  ],
  "reasoning": "These alternatives provide reversible paths forward",
  "recommendation": "Alternative approach 1"
}
```
EOF

    echo "0|5000|$output_file"
}

# Cleanup function
cleanup() {
    rm -rf "$NEEDLE_HOME"
    rm -rf "$NEEDLE_WORKSPACE"
}
trap cleanup EXIT

# Run tests
echo "=========================================="
echo "Running strands/unravel.sh tests"
echo "=========================================="

# Test 1: Is enabled check works
_test_start "Is enabled check returns true when enabled"
if _needle_unravel_is_enabled; then
    _test_pass "Is enabled check returns true when enabled"
else
    _test_fail "Is enabled check returned false when should be true"
fi

# Test 2: Get min wait hours from config
_test_start "Get min wait hours from config"
min_wait=$(_needle_unravel_get_min_wait_hours)
if [[ "$min_wait" == "24" ]]; then
    _test_pass "Min wait hours read correctly: $min_wait"
else
    _test_fail "Min wait hours incorrect: expected 24, got $min_wait"
fi

# Test 3: Get max alternatives from config
_test_start "Get max alternatives from config"
max_alts=$(_needle_unravel_get_max_alternatives)
if [[ "$max_alts" == "3" ]]; then
    _test_pass "Max alternatives read correctly: $max_alts"
else
    _test_fail "Max alternatives incorrect: expected 3, got $max_alts"
fi

# Test 4: Get timeout from config
_test_start "Get timeout from config"
timeout=$(_needle_unravel_get_timeout)
if [[ "$timeout" == "120" ]]; then
    _test_pass "Timeout read correctly: $timeout"
else
    _test_fail "Timeout incorrect: expected 120, got $timeout"
fi

# Test 5: Count alternatives returns correct count
_test_start "Count alternatives returns correct count"
count=$(_needle_unravel_count_alternatives "$NEEDLE_WORKSPACE" "nd-human-old")
if [[ "$count" == "0" ]]; then
    _test_pass "Count alternatives returns 0 when no alternatives exist"
else
    _test_fail "Count alternatives should return 0, got $count"
fi

# Test 6: Build prompt includes bead details
_test_start "Build prompt includes bead details"
prompt=$(_needle_unravel_build_prompt "nd-test-id" "$NEEDLE_WORKSPACE" '{"title": "Test Bead", "description": "Test description"}')
if echo "$prompt" | grep -q "nd-test-id" && echo "$prompt" | grep -q "Test Bead"; then
    _test_pass "Build prompt includes bead ID and title"
else
    _test_fail "Build prompt missing expected content"
fi

# Test 7: Build prompt includes max alternatives constraint
_test_start "Build prompt includes max alternatives constraint"
if echo "$prompt" | grep -q "Propose 1-3 alternative"; then
    _test_pass "Build prompt includes max alternatives constraint"
else
    _test_fail "Build prompt missing max alternatives constraint"
fi

# Test 8: Parse alternatives from JSON output
_test_start "Parse alternatives from JSON output"
test_output='```json
{
  "alternatives": [
    {"title": "Test Alt", "description": "Test", "approach": "Test approach"}
  ]
}
```'
alts=$(_needle_unravel_parse_alternatives "$test_output")
if echo "$alts" | grep -q "Test Alt" || echo "$alts" | grep -q "title"; then
    _test_pass "Parse alternatives extracts JSON correctly"
else
    _test_fail "Parse alternatives failed to extract: $alts"
fi

# Test 9: Parse alternatives handles empty output
_test_start "Parse alternatives handles empty output"
alts=$(_needle_unravel_parse_alternatives "No JSON here")
if [[ "$alts" == "[]" ]]; then
    _test_pass "Parse alternatives returns empty array for invalid input"
else
    _test_fail "Parse alternatives should return [], got: $alts"
fi

# Test 10: Stats function returns valid JSON
_test_start "Stats function returns valid JSON"
stats=$(_needle_unravel_stats)
if echo "$stats" | jq -e . >/dev/null 2>&1; then
    _test_pass "Stats function returns valid JSON"
else
    _test_fail "Stats function returned invalid JSON: $stats"
fi

# Test 11: Stats function includes expected fields
_test_start "Stats function includes expected fields"
stats=$(_needle_unravel_stats)
if echo "$stats" | jq -e ".enabled != null" >/dev/null 2>&1 && echo "$stats" | jq -e ".min_wait_hours != null" >/dev/null 2>&1; then
    _test_pass "Stats function includes expected fields"
else
    _test_fail "Stats function missing expected fields"
fi

# Test 12: Strand function handles no blocked beads gracefully
# Override br mock for this test
br() {
    case "$1" in
        list)
            echo '[]'
            ;;
        *)
            return 0
            ;;
    esac
}
_test_start "Strand function handles no blocked beads gracefully"
if ! _needle_strand_unravel "$NEEDLE_WORKSPACE" "test-agent"; then
    _test_pass "Strand correctly returns failure when no blocked beads"
else
    _test_fail "Strand should return failure when no blocked beads"
fi

# Test 13: Strand respects min_wait_hours
# Create a scenario where all beads are too recent
br() {
    case "$1" in
        list)
            if [[ "$*" == *"--status blocked"* ]] && [[ "$*" == *"--type human"* ]]; then
                local now=$(date +%s)
                local recent_ts=$((now - 3600))  # Only 1 hour ago
                cat << EOF
[{
  "id": "nd-human-recent",
  "title": "Recent HUMAN bead",
  "description": "This bead is too recent",
  "status": "blocked",
  "issue_type": "human",
  "created_at": "$recent_ts"
}]
EOF
            else
                echo '[]'
            fi
            ;;
        *)
            return 0
            ;;
    esac
}
_test_start "Strand respects min_wait_hours for recent beads"
if ! _needle_strand_unravel "$NEEDLE_WORKSPACE" "test-agent"; then
    _test_pass "Strand correctly skips beads that haven't waited long enough"
else
    _test_fail "Strand should skip beads that haven't waited long enough"
fi

# Test 14: Strand creates alternatives for old beads
# Reset br mock to return old beads
br() {
    case "$1" in
        list)
            if [[ "$*" == *"--status blocked"* ]] && [[ "$*" == *"--type human"* ]]; then
                local now=$(date +%s)
                local old_ts=$((now - 86400))  # 24 hours ago
                cat << EOF
[{
  "id": "nd-human-old",
  "title": "Old HUMAN bead",
  "description": "This bead has waited long enough",
  "status": "blocked",
  "issue_type": "human",
  "created_at": "$old_ts"
}]
EOF
            elif [[ "$*" == *"--label"* ]]; then
                echo '[]'
            else
                echo '[]'
            fi
            ;;
        create)
            echo "nd-alternative-$$"
            return 0
            ;;
        *)
            return 0
            ;;
    esac
}
_test_start "Strand creates alternatives for old beads"
if _needle_strand_unravel "$NEEDLE_WORKSPACE" "test-agent"; then
    _test_pass "Strand correctly creates alternatives for old beads"
else
    _test_fail "Strand should create alternatives for old beads"
fi

# Test 15: Disabled strand returns failure
# Create config with unravel disabled
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
strands:
  unravel: false

unravel:
  min_wait_hours: 24
EOF
# Clear config cache
clear_config_cache
_test_start "Disabled strand returns failure"
if ! _needle_strand_unravel "$NEEDLE_WORKSPACE" "test-agent"; then
    _test_pass "Disabled strand correctly returns failure"
else
    _test_fail "Disabled strand should return failure"
fi

# Reset config
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
strands:
  pluck: true
  explore: true
  mend: true
  weave: false
  unravel: true
  pulse: false
  knot: true

unravel:
  min_wait_hours: 24
  max_alternatives: 3
  timeout: 120
EOF
clear_config_cache

# Test 16: Create alternatives handles max limit
_test_start "Create alternatives respects max limit"
# Create mock alternatives JSON with more than max
mock_alts='[
  {"title": "Alt 1", "description": "D1", "approach": "A1", "reversibility": "Easy to undo", "tradeoffs": "Low risk"},
  {"title": "Alt 2", "description": "D2", "approach": "A2", "reversibility": "Easy to undo", "tradeoffs": "Medium risk"},
  {"title": "Alt 3", "description": "D3", "approach": "A3", "reversibility": "Easy to undo", "tradeoffs": "Low risk"},
  {"title": "Alt 4", "description": "D4", "approach": "A4", "reversibility": "Easy to undo", "tradeoffs": "Medium risk"},
  {"title": "Alt 5", "description": "D5", "approach": "A5", "reversibility": "Easy to undo", "tradeoffs": "Low risk"}
]'
# This should only create 3 (max_alternatives)
# Capture only the last line which is the count
created=$(_needle_unravel_create_alternatives "$NEEDLE_WORKSPACE" "nd-test" "$mock_alts" 2>/dev/null | tail -1)
if [[ "$created" =~ ^[0-9]+$ ]] && [[ "$created" -le 3 ]]; then
    _test_pass "Create alternatives respects max limit (created $created)"
else
    _test_fail "Create alternatives exceeded max limit: created $created"
fi

# Test 17: Alternative titles get [ALTERNATIVE] prefix
_test_start "Alternative titles get [ALTERNATIVE] prefix"
# Use a temp file to capture arguments from subshell
CAPTURE_FILE=$(mktemp)
br() {
    case "$1" in
        create)
            # Write all args to capture file
            echo "$@" > "$CAPTURE_FILE"
            echo "nd-alternative-prefix-$$"
            return 0
            ;;
        list)
            echo '[]'
            ;;
        *)
            return 0
            ;;
    esac
}
mock_alts='[{"title": "Test Alternative", "description": "Test", "approach": "Test approach", "reversibility": "Easy", "tradeoffs": "None"}]'
_needle_unravel_create_alternatives "$NEEDLE_WORKSPACE" "nd-parent-test" "$mock_alts" 2>/dev/null | tail -1 >/dev/null
# Check if [ALTERNATIVE] prefix was added to title
if grep -q "\-\-title \[ALTERNATIVE\] Test Alternative" "$CAPTURE_FILE"; then
    _test_pass "Alternative title has [ALTERNATIVE] prefix"
else
    _test_fail "Alternative title missing prefix. Got: $(cat "$CAPTURE_FILE")"
fi
rm -f "$CAPTURE_FILE"

# Test 18: Alternative beads use --parent option
_test_start "Alternative beads use --parent option for relationship"
CAPTURE_FILE=$(mktemp)
br() {
    case "$1" in
        create)
            echo "$@" > "$CAPTURE_FILE"
            echo "nd-alternative-parent-$$"
            return 0
            ;;
        list)
            echo '[]'
            ;;
        *)
            return 0
            ;;
    esac
}
mock_alts='[{"title": "Test With Parent", "description": "Test", "approach": "Test approach", "reversibility": "Easy", "tradeoffs": "None"}]'
_needle_unravel_create_alternatives "$NEEDLE_WORKSPACE" "nd-parent-123" "$mock_alts" 2>/dev/null | tail -1 >/dev/null
if grep -q "\-\-parent nd-parent-123" "$CAPTURE_FILE"; then
    _test_pass "Alternative bead uses --parent option"
else
    _test_fail "Alternative bead missing --parent option. Got: $(cat "$CAPTURE_FILE")"
fi
rm -f "$CAPTURE_FILE"

# Test 19: Prompt template includes plan.md required fields
_test_start "Prompt template includes plan.md required fields"
prompt=$(_needle_unravel_build_prompt "nd-test-id" "$NEEDLE_WORKSPACE" '{"title": "Test Bead", "description": "Test description", "created_at": "1234567890"}')
if echo "$prompt" | grep -q "Blocked Bead" && \
   echo "$prompt" | grep -q "Waiting Since" && \
   echo "$prompt" | grep -q "reversibility" && \
   echo "$prompt" | grep -q "tradeoffs" && \
   echo "$prompt" | grep -q "parent_bead"; then
    _test_pass "Prompt template includes required plan.md fields"
else
    _test_fail "Prompt template missing required plan.md fields"
fi

# Test 20: Alternatives without prefix get prefix added
_test_start "Alternatives without [ALTERNATIVE] prefix get prefix added"
CAPTURE_FILE=$(mktemp)
br() {
    case "$1" in
        create)
            echo "$@" > "$CAPTURE_FILE"
            echo "nd-alt-auto-prefix-$$"
            return 0
            ;;
        list)
            echo '[]'
            ;;
        *)
            return 0
            ;;
    esac
}
# Create alternative without [ALTERNATIVE] prefix - it should be added
mock_alts='[{"title": "Auto prefixed alternative", "description": "Test", "approach": "Test", "reversibility": "Easy", "tradeoffs": "None"}]'
_needle_unravel_create_alternatives "$NEEDLE_WORKSPACE" "nd-parent" "$mock_alts" 2>/dev/null | tail -1 >/dev/null
if grep -q "\-\-title \[ALTERNATIVE\] Auto prefixed alternative" "$CAPTURE_FILE"; then
    _test_pass "Title without prefix gets [ALTERNATIVE] added"
else
    _test_fail "Title not properly prefixed. Got: $(cat "$CAPTURE_FILE")"
fi
rm -f "$CAPTURE_FILE"

# Test 21: Alternatives already with prefix keep prefix
_test_start "Alternatives already with [ALTERNATIVE] prefix keep prefix"
CAPTURE_FILE=$(mktemp)
br() {
    case "$1" in
        create)
            echo "$@" > "$CAPTURE_FILE"
            echo "nd-alt-existing-prefix-$$"
            return 0
            ;;
        list)
            echo '[]'
            ;;
        *)
            return 0
            ;;
    esac
}
# Create alternative that already has [ALTERNATIVE] prefix - it should NOT be duplicated
mock_alts='[{"title": "[ALTERNATIVE] Already prefixed", "description": "Test", "approach": "Test", "reversibility": "Easy", "tradeoffs": "None"}]'
_needle_unravel_create_alternatives "$NEEDLE_WORKSPACE" "nd-parent" "$mock_alts" 2>/dev/null | tail -1 >/dev/null
# Should have exactly one [ALTERNATIVE] prefix (not duplicated)
if grep -q "\-\-title \[ALTERNATIVE\] Already prefixed" "$CAPTURE_FILE"; then
    # Make sure it's not duplicated
    prefix_count=$(grep -o "\[ALTERNATIVE\]" "$CAPTURE_FILE" | wc -l)
    if [[ "$prefix_count" -eq 1 ]]; then
        _test_pass "Title with existing prefix not duplicated"
    else
        _test_fail "Prefix was duplicated. Count: $prefix_count. Got: $(cat "$CAPTURE_FILE")"
    fi
else
    _test_fail "Title improperly handled. Got: $(cat "$CAPTURE_FILE")"
fi
rm -f "$CAPTURE_FILE"

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
