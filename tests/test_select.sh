#!/usr/bin/env bash
# Tests for NEEDLE bead selection module (src/bead/select.sh)

# Test setup - create temp directory
TEST_DIR=$(mktemp -d)
TEST_NEEDLE_HOME="$TEST_DIR/.needle"

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_HOME="$TEST_NEEDLE_HOME"
export NEEDLE_STATE_DIR="state"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=false

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/bead/select.sh"

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

# Mock br ready command for testing
mock_br_ready() {
    local data="$1"
    # Create a mock br script
    mkdir -p "$TEST_DIR/bin"
    cat > "$TEST_DIR/bin/br" << EOF
#!/bin/bash
case "\$1 \$2" in
    "ready --unassigned")
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

echo "=== NEEDLE Bead Selection Tests ==="
echo ""

# Test 1: Priority weight calculation
test_case "Priority weight P0 returns 8"
weight=$(_needle_get_priority_weight 0)
if [[ "$weight" == "8" ]]; then
    test_pass
else
    test_fail "Expected 8, got $weight"
fi

test_case "Priority weight P1 returns 4"
weight=$(_needle_get_priority_weight 1)
if [[ "$weight" == "4" ]]; then
    test_pass
else
    test_fail "Expected 4, got $weight"
fi

test_case "Priority weight P2 returns 2"
weight=$(_needle_get_priority_weight 2)
if [[ "$weight" == "2" ]]; then
    test_pass
else
    test_fail "Expected 2, got $weight"
fi

test_case "Priority weight P3 returns 1"
weight=$(_needle_get_priority_weight 3)
if [[ "$weight" == "1" ]]; then
    test_pass
else
    test_fail "Expected 1, got $weight"
fi

test_case "Priority weight P4+ returns 1 (capped)"
weight=$(_needle_get_priority_weight 4)
if [[ "$weight" == "1" ]]; then
    test_pass
else
    test_fail "Expected 1, got $weight"
fi

test_case "Priority weight default (no arg) returns 2"
weight=$(_needle_get_priority_weight)
if [[ "$weight" == "2" ]]; then
    test_pass
else
    test_fail "Expected 2, got $weight"
fi

test_case "Priority weight invalid returns 2 (default)"
weight=$(_needle_get_priority_weight "invalid")
if [[ "$weight" == "2" ]]; then
    test_pass
else
    test_fail "Expected 2, got $weight"
fi

# Test 2: Empty queue handling
test_case "Returns error on empty queue"
mock_br_ready '[]'
if ! _needle_select_weighted &>/dev/null; then
    test_pass
else
    test_fail "Expected failure on empty queue"
fi

test_case "Returns error on null response"
mock_br_ready 'null'
if ! _needle_select_weighted &>/dev/null; then
    test_pass
else
    test_fail "Expected failure on null response"
fi

test_case "Returns error on empty string response"
mock_br_ready ''
if ! _needle_select_weighted &>/dev/null; then
    test_pass
else
    test_fail "Expected failure on empty string"
fi

# Test 3: Single bead selection
test_case "Selects single bead correctly"
mock_br_ready '[{"id":"bd-test1","title":"Test Bead","priority":2}]'
result=$(_needle_select_weighted 2>/dev/null)
if [[ "$result" == "bd-test1" ]]; then
    test_pass
else
    test_fail "Expected bd-test1, got $result"
fi

test_case "Outputs JSON with --json flag"
mock_br_ready '[{"id":"bd-test1","title":"Test Bead","priority":2}]'
result=$(_needle_select_weighted --json 2>/dev/null)
if echo "$result" | jq -e '.id == "bd-test1"' &>/dev/null; then
    test_pass
else
    test_fail "Expected JSON with id bd-test1, got $result"
fi

# Test 4: Multiple beads with different priorities
test_case "Selects from multiple beads with different priorities"
mock_br_ready '[{"id":"bd-p0","priority":0},{"id":"bd-p1","priority":1},{"id":"bd-p2","priority":2}]'

# Run selection multiple times and verify we get valid IDs
valid=true
for i in {1..10}; do
    result=$(_needle_select_weighted 2>/dev/null)
    if [[ "$result" != "bd-p0" ]] && [[ "$result" != "bd-p1" ]] && [[ "$result" != "bd-p2" ]]; then
        valid=false
        break
    fi
done

if $valid; then
    test_pass
else
    test_fail "Got invalid bead ID: $result"
fi

# Test 5: Weighted selection favors higher priority (statistical test)
test_case "Higher priority beads selected more frequently"

# Create test beads with different priorities
mock_br_ready '[{"id":"bd-high","priority":0},{"id":"bd-low","priority":3}]'

# Run selection 100 times
declare -A counts
for i in {1..100}; do
    result=$(_needle_select_weighted 2>/dev/null)
    ((counts[$result]++))
done

# P0 (weight 8) should be selected ~8x more than P3 (weight 1)
# With 100 selections and weights 8:1, we expect ~89 P0 and ~11 P3
# Allow significant margin for randomness
high_count=${counts[bd-high]:-0}
low_count=${counts[bd-low]:-0}

if [[ $high_count -gt $low_count ]]; then
    test_pass "(high:$high_count vs low:$low_count)"
else
    test_fail "Expected more high priority selections (high:$high_count vs low:$low_count)"
fi

# Test 6: List weighted beads
test_case "Lists all beads with weights"
mock_br_ready '[{"id":"bd-1","priority":0},{"id":"bd-2","priority":2}]'
result=$(_needle_list_weighted_beads)

if echo "$result" | jq -e 'length == 2' &>/dev/null && \
   echo "$result" | jq -e '.[0].weight == 8' &>/dev/null && \
   echo "$result" | jq -e '.[1].weight == 2' &>/dev/null; then
    test_pass
else
    test_fail "Expected 2 beads with weights 8 and 2"
fi

# Test 7: Selection statistics
test_case "Generates selection statistics"
mock_br_ready '[{"id":"bd-1","priority":0},{"id":"bd-2","priority":0},{"id":"bd-3","priority":2}]'
result=$(_needle_select_stats)

if echo "$result" | jq -e '.total_beads == 3' &>/dev/null && \
   echo "$result" | jq -e '.weighted_pool_size == 18' &>/dev/null && \
   echo "$result" | jq -e '.by_priority.P0.count == 2' &>/dev/null; then
    test_pass
else
    test_fail "Expected total_beads=3, weighted_pool_size=18 (8+8+2), got: $result"
fi

# Test 8: Bead without priority defaults to P2
test_case "Bead without priority defaults to P2 (weight 2)"
mock_br_ready '[{"id":"bd-default"}]'
result=$(_needle_list_weighted_beads)

if echo "$result" | jq -e '.[0].weight == 2' &>/dev/null; then
    test_pass
else
    test_fail "Expected weight 2 for bead without priority"
fi

# Test 9: Invalid JSON handling
test_case "Returns error on invalid JSON"
mock_br_ready 'not valid json'
if ! _needle_select_weighted &>/dev/null; then
    test_pass
else
    test_fail "Expected failure on invalid JSON"
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
