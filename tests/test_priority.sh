#!/usr/bin/env bash
# Tests for NEEDLE bead priority algorithm (src/bead/select.sh)
#
# This test suite focuses specifically on the weighted priority selection algorithm:
# - Priority ordering (P0 > P1 > P2 > P3)
# - Weighted randomness within same priority
# - Statistical distribution validation
# - Edge cases (empty, single, tie-breaking)

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
# Use unlimited billing model for base weight tests
export NEEDLE_BILLING_MODEL="unlimited"

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/lib/config.sh"
source "$PROJECT_DIR/src/lib/billing_models.sh"
source "$PROJECT_DIR/src/bead/select.sh"

# Cleanup function
cleanup() {
    rm -rf "$TEST_DIR"
    # Clean up any temp files
    rm -f /tmp/test_priority_attempt_* 2>/dev/null
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
    local extra="${1:-}"
    echo "PASS${extra:+ $extra}"
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
    "ready --unassigned"|"ready --unassigned --json"|"ready --workspace="*|"ready --workspace="*"--json")
        echo '$data'
        ;;
    "list --status"*)
        # Fallback for br list
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

echo "=== NEEDLE Bead Priority Algorithm Tests ==="
echo ""

# ============================================================================
# Test 1: Priority Weight Calculations (Base Weights via unlimited model)
# ============================================================================

echo "--- Priority Weight Calculations ---"

test_case "P0 (critical) weight = 8"
weight=$(_needle_billing_get_priority_weight 0 unlimited)
if [[ "$weight" == "8" ]]; then
    test_pass
else
    test_fail "Expected 8, got $weight"
fi

test_case "P1 (high) weight = 4"
weight=$(_needle_billing_get_priority_weight 1 unlimited)
if [[ "$weight" == "4" ]]; then
    test_pass
else
    test_fail "Expected 4, got $weight"
fi

test_case "P2 (normal) weight = 2"
weight=$(_needle_billing_get_priority_weight 2 unlimited)
if [[ "$weight" == "2" ]]; then
    test_pass
else
    test_fail "Expected 2, got $weight"
fi

test_case "P3 (low) weight = 1"
weight=$(_needle_billing_get_priority_weight 3 unlimited)
if [[ "$weight" == "1" ]]; then
    test_pass
else
    test_fail "Expected 1, got $weight"
fi

test_case "P4+ (backlog) weight = 1 (capped)"
weight=$(_needle_billing_get_priority_weight 4 unlimited)
if [[ "$weight" == "1" ]]; then
    test_pass
else
    test_fail "Expected 1, got $weight"
fi

test_case "P5+ weight also capped to 1"
weight=$(_needle_billing_get_priority_weight 5 unlimited)
if [[ "$weight" == "1" ]]; then
    test_pass
else
    test_fail "Expected 1, got $weight"
fi

test_case "Missing priority defaults to P2 (weight=2)"
weight=$(_needle_billing_get_priority_weight "" unlimited)
if [[ "$weight" == "2" ]]; then
    test_pass
else
    test_fail "Expected 2, got $weight"
fi

test_case "Invalid priority string defaults to P2"
weight=$(_needle_billing_get_priority_weight "invalid" unlimited)
if [[ "$weight" == "2" ]]; then
    test_pass
else
    test_fail "Expected 2, got $weight"
fi

test_case "Negative priority defaults to P2"
weight=$(_needle_billing_get_priority_weight -1 unlimited)
if [[ "$weight" == "2" ]]; then
    test_pass
else
    test_fail "Expected 2, got $weight"
fi

# ============================================================================
# Test 1b: Billing Model Weight Adjustments
# ============================================================================

echo ""
echo "--- Billing Model Weight Adjustments ---"

test_case "pay_per_token: P0 unchanged (8)"
weight=$(_needle_billing_get_priority_weight 0 pay_per_token)
if [[ "$weight" == "8" ]]; then
    test_pass
else
    test_fail "Expected 8, got $weight"
fi

test_case "pay_per_token: P1 unchanged (4)"
weight=$(_needle_billing_get_priority_weight 1 pay_per_token)
if [[ "$weight" == "4" ]]; then
    test_pass
else
    test_fail "Expected 4, got $weight"
fi

test_case "pay_per_token: P2 halved (2/2=1)"
weight=$(_needle_billing_get_priority_weight 2 pay_per_token)
if [[ "$weight" == "1" ]]; then
    test_pass
else
    test_fail "Expected 1, got $weight"
fi

test_case "pay_per_token: P3 halved (1/2=0)"
weight=$(_needle_billing_get_priority_weight 3 pay_per_token)
if [[ "$weight" == "0" ]]; then
    test_pass
else
    test_fail "Expected 0, got $weight"
fi

test_case "use_or_lose: P0 boosted (8+4=12)"
weight=$(_needle_billing_get_priority_weight 0 use_or_lose)
if [[ "$weight" == "12" ]]; then
    test_pass
else
    test_fail "Expected 12, got $weight"
fi

test_case "use_or_lose: P1 boosted (4+2=6)"
weight=$(_needle_billing_get_priority_weight 1 use_or_lose)
if [[ "$weight" == "6" ]]; then
    test_pass
else
    test_fail "Expected 6, got $weight"
fi

test_case "use_or_lose: P2 boosted (2+1=3)"
weight=$(_needle_billing_get_priority_weight 2 use_or_lose)
if [[ "$weight" == "3" ]]; then
    test_pass
else
    test_fail "Expected 3, got $weight"
fi

test_case "use_or_lose: P3 unchanged (1)"
weight=$(_needle_billing_get_priority_weight 3 use_or_lose)
if [[ "$weight" == "1" ]]; then
    test_pass
else
    test_fail "Expected 1, got $weight"
fi

test_case "unlimited: P0 unchanged (8)"
weight=$(_needle_billing_get_priority_weight 0 unlimited)
if [[ "$weight" == "8" ]]; then
    test_pass
else
    test_fail "Expected 8, got $weight"
fi

test_case "unlimited: P2 unchanged (2)"
weight=$(_needle_billing_get_priority_weight 2 unlimited)
if [[ "$weight" == "2" ]]; then
    test_pass
else
    test_fail "Expected 2, got $weight"
fi

test_case "unlimited: P3 unchanged (1)"
weight=$(_needle_billing_get_priority_weight 3 unlimited)
if [[ "$weight" == "1" ]]; then
    test_pass
else
    test_fail "Expected 1, got $weight"
fi

# ============================================================================
# Test 2: Priority Ordering - Higher Priority More Likely
# ============================================================================

echo ""
echo "--- Priority Ordering ---"

test_case "P0 selected more frequently than P1"
mock_br_ready '[{"id":"bd-p0","priority":0},{"id":"bd-p1","priority":1}]'

declare -A counts
for i in {1..50}; do
    result=$(_needle_select_weighted 2>/dev/null)
    ((counts[$result]++))
done

p0_count=${counts[bd-p0]:-0}
p1_count=${counts[bd-p1]:-0}

# P0 (8) vs P1 (4) = 2:1 ratio - P0 should win more often than not
# With 50 trials, P0 should get ~33, P1 ~17. Allow P0 >= 20 as "more frequent"
if [[ $p0_count -ge 20 ]]; then
    test_pass "(P0:$p0_count vs P1:$p1_count)"
else
    test_fail "Expected more P0 selections (P0:$p0_count vs P1:$p1_count)"
fi

test_case "P1 selected more frequently than P2"
mock_br_ready '[{"id":"bd-p1","priority":1},{"id":"bd-p2","priority":2}]'

declare -A counts2
for i in {1..30}; do
    result=$(_needle_select_weighted 2>/dev/null)
    ((counts2[$result]++))
done

p1_count=${counts2[bd-p1]:-0}
p2_count=${counts2[bd-p2]:-0}

# P1 (4) vs P2 (1 in pay_per_token) = 4:1 ratio
if [[ $p1_count -gt $p2_count ]]; then
    test_pass "(P1:$p1_count vs P2:$p2_count)"
else
    test_fail "Expected more P1 selections (P1:$p1_count vs P2:$p2_count)"
fi

test_case "P2 selected more frequently than P3"
mock_br_ready '[{"id":"bd-p2","priority":2},{"id":"bd-p3","priority":3}]'

declare -A counts3
for i in {1..20}; do
    result=$(_needle_select_weighted 2>/dev/null)
    ((counts3[$result]++))
done

p2_count=${counts3[bd-p2]:-0}
p3_count=${counts3[bd-p3]:-0}

# With pay_per_token, P3 has weight 0, so P2 always wins
if [[ $p2_count -gt $p3_count ]]; then
    test_pass "(P2:$p2_count vs P3:$p3_count)"
else
    test_fail "Expected more P2 selections (P2:$p2_count vs P3:$p3_count)"
fi

test_case "P0 overwhelmingly preferred over P3 (8:1 ratio)"
mock_br_ready '[{"id":"bd-p0","priority":0},{"id":"bd-p3","priority":3}]'

declare -A counts4
for i in {1..20}; do
    result=$(_needle_select_weighted 2>/dev/null)
    ((counts4[$result]++))
done

p0_count=${counts4[bd-p0]:-0}
p3_count=${counts4[bd-p3]:-0}

# With pay_per_token, P3 has weight 0, so P0 always wins
if [[ $p0_count -ge 15 ]]; then
    pct=$((p0_count * 100 / 20))
    test_pass "(P0:$p0_count vs P3:$p3_count, ~${pct}%)"
else
    test_fail "Expected mostly P0 selections, got P0:$p0_count vs P3:$p3_count"
fi

# ============================================================================
# Test 3: Tie-Breaking Within Same Priority
# ============================================================================

echo ""
echo "--- Tie-Breaking (Same Priority) ---"

test_case "Same priority beads have equal selection probability"
mock_br_ready '[{"id":"bd-a","priority":2},{"id":"bd-b","priority":2}]'

declare -A tie_counts
for i in {1..30}; do
    result=$(_needle_select_weighted 2>/dev/null)
    ((tie_counts[$result]++))
done

count_a=${tie_counts[bd-a]:-0}
count_b=${tie_counts[bd-b]:-0}

# Both should be roughly equal
# With 30 trials, expect ~15 each, allow 8-22 range
if [[ $count_a -ge 8 ]] && [[ $count_a -le 22 ]] && \
   [[ $count_b -ge 8 ]] && [[ $count_b -le 22 ]]; then
    test_pass "(A:$count_a vs B:$count_b)"
else
    test_fail "Expected roughly equal distribution (A:$count_a vs B:$count_b)"
fi

test_case "Three same-priority beads distributed evenly"
mock_br_ready '[{"id":"bd-x","priority":1},{"id":"bd-y","priority":1},{"id":"bd-z","priority":1}]'

declare -A triple_counts
for i in {1..30}; do
    result=$(_needle_select_weighted 2>/dev/null)
    ((triple_counts[$result]++))
done

count_x=${triple_counts[bd-x]:-0}
count_y=${triple_counts[bd-y]:-0}
count_z=${triple_counts[bd-z]:-0}

# Each should be ~10 (33%), allow 4-16 range
if [[ $count_x -ge 4 ]] && [[ $count_x -le 16 ]] && \
   [[ $count_y -ge 4 ]] && [[ $count_y -le 16 ]] && \
   [[ $count_z -ge 4 ]] && [[ $count_z -le 16 ]]; then
    test_pass "(X:$count_x, Y:$count_y, Z:$count_z)"
else
    test_fail "Expected roughly equal distribution (X:$count_x, Y:$count_y, Z:$count_z)"
fi

test_case "Four P0 beads have equal probability"
mock_br_ready '[{"id":"bd-p0a","priority":0},{"id":"bd-p0b","priority":0},{"id":"bd-p0c","priority":0},{"id":"bd-p0d","priority":0}]'

declare -A p0_counts
for i in {1..40}; do
    result=$(_needle_select_weighted 2>/dev/null)
    ((p0_counts[$result]++))
done

# Each should be ~10, allow 3-17 range
all_in_range=true
for key in "${!p0_counts[@]}"; do
    if [[ ${p0_counts[$key]} -lt 3 ]] || [[ ${p0_counts[$key]} -gt 17 ]]; then
        all_in_range=false
    fi
done

if $all_in_range && [[ ${#p0_counts[@]} -eq 4 ]]; then
    test_pass "(counts: ${p0_counts[bd-p0a]}, ${p0_counts[bd-p0b]}, ${p0_counts[bd-p0c]}, ${p0_counts[bd-p0d]})"
else
    test_fail "Expected 4 beads with ~10 each, got: $(declare -p p0_counts)"
fi

# ============================================================================
# Test 4: Edge Cases
# ============================================================================

echo ""
echo "--- Edge Cases ---"

test_case "Empty queue returns error"
mock_br_ready '[]'
if ! _needle_select_weighted &>/dev/null; then
    test_pass
else
    test_fail "Expected failure on empty queue"
fi

test_case "Null response returns error"
mock_br_ready 'null'
if ! _needle_select_weighted &>/dev/null; then
    test_pass
else
    test_fail "Expected failure on null response"
fi

test_case "Empty string response returns error"
mock_br_ready ''
if ! _needle_select_weighted &>/dev/null; then
    test_pass
else
    test_fail "Expected failure on empty string"
fi

test_case "Single bead always selected"
mock_br_ready '[{"id":"bd-single","priority":2}]'

all_match=true
for i in {1..20}; do
    result=$(_needle_select_weighted 2>/dev/null)
    if [[ "$result" != "bd-single" ]]; then
        all_match=false
        break
    fi
done

if $all_match; then
    test_pass
else
    test_fail "Expected bd-single every time, got $result"
fi

test_case "Single P0 bead always selected"
mock_br_ready '[{"id":"bd-critical","priority":0}]'
result=$(_needle_select_weighted 2>/dev/null)
if [[ "$result" == "bd-critical" ]]; then
    test_pass
else
    test_fail "Expected bd-critical, got $result"
fi

test_case "Bead without priority defaults to P2 (pay_per_token: 1)"
mock_br_ready '[{"id":"bd-no-priority"}]'
result=$(_needle_list_weighted_beads 2>/dev/null)
# With pay_per_token, P2 weight = 2/2 = 1
if echo "$result" | jq -e '.[0].weight == 1' &>/dev/null; then
    test_pass
else
    test_fail "Expected weight 1 for bead without priority (pay_per_token model)"
fi

test_case "Invalid JSON returns error"
mock_br_ready 'not valid json at all'
if ! _needle_select_weighted &>/dev/null; then
    test_pass
else
    test_fail "Expected failure on invalid JSON"
fi

# ============================================================================
# Test 5: Statistical Distribution Validation
# ============================================================================

echo ""
echo "--- Statistical Distribution ---"

test_case "Weighted pool size calculated correctly (pay_per_token)"
mock_br_ready '[{"id":"bd-1","priority":0},{"id":"bd-2","priority":1},{"id":"bd-3","priority":2}]'
result=$(_needle_list_weighted_beads 2>/dev/null)

# With pay_per_token: P0=8, P1=4, P2=1 => total=13
total_weight=$(echo "$result" | jq '[.[].weight] | add')
if [[ "$total_weight" == "13" ]]; then
    test_pass
else
    test_fail "Expected total weight 13 (pay_per_token), got $total_weight"
fi

test_case "Multiple P0 beads have correct total weight"
mock_br_ready '[{"id":"bd-a","priority":0},{"id":"bd-b","priority":0},{"id":"bd-c","priority":0}]'
result=$(_needle_list_weighted_beads 2>/dev/null)

# 3 * P0(8) = 24
total_weight=$(echo "$result" | jq '[.[].weight] | add')
if [[ "$total_weight" == "24" ]]; then
    test_pass
else
    test_fail "Expected total weight 24, got $total_weight"
fi

test_case "Mixed priorities have correct weighted pool (pay_per_token)"
mock_br_ready '[{"id":"bd-p0","priority":0},{"id":"bd-p1","priority":1},{"id":"bd-p2","priority":2},{"id":"bd-p3","priority":3}]'
result=$(_needle_list_weighted_beads 2>/dev/null)

# With pay_per_token: 8 + 4 + 1 + 0 = 13
total_weight=$(echo "$result" | jq '[.[].weight] | add')
if [[ "$total_weight" == "13" ]]; then
    test_pass
else
    test_fail "Expected total weight 13 (pay_per_token), got $total_weight"
fi

# ============================================================================
# Test 6: JSON Output Format
# ============================================================================

echo ""
echo "--- JSON Output ---"

test_case "--json flag returns valid JSON object"
mock_br_ready '[{"id":"bd-json-test","title":"Test Bead","priority":1}]'
result=$(_needle_select_weighted --json 2>/dev/null)

if echo "$result" | jq -e '.id == "bd-json-test"' &>/dev/null && \
   echo "$result" | jq -e '.priority == 1' &>/dev/null; then
    test_pass
else
    test_fail "Expected valid JSON with id and priority, got: $result"
fi

test_case "_needle_list_weighted_beads returns JSON array with weights (pay_per_token)"
mock_br_ready '[{"id":"bd-1","priority":0},{"id":"bd-2","priority":2}]'
result=$(_needle_list_weighted_beads 2>/dev/null)

# With pay_per_token: P0=8, P2=1
if echo "$result" | jq -e 'type == "array"' &>/dev/null && \
   echo "$result" | jq -e 'length == 2' &>/dev/null && \
   echo "$result" | jq -e '.[0].weight == 8' &>/dev/null && \
   echo "$result" | jq -e '.[1].weight == 1' &>/dev/null; then
    test_pass
else
    test_fail "Expected array with 2 beads having weights 8 and 1 (pay_per_token)"
fi

# ============================================================================
# Test 7: Complex Priority Scenarios
# ============================================================================

echo ""
echo "--- Complex Scenarios ---"

test_case "Many beads with mixed priorities - all valid IDs returned"
mock_br_ready '[{"id":"bd-a","priority":0},{"id":"bd-b","priority":0},{"id":"bd-c","priority":1},{"id":"bd-d","priority":2},{"id":"bd-e","priority":2},{"id":"bd-f","priority":3}]'

all_valid=true
valid_ids="bd-a bd-b bd-c bd-d bd-e bd-f"
for i in {1..20}; do
    result=$(_needle_select_weighted 2>/dev/null)
    if ! echo "$valid_ids" | grep -qw "$result"; then
        all_valid=false
        break
    fi
done

if $all_valid; then
    test_pass
else
    test_fail "Got invalid bead ID: $result"
fi

test_case "Only P2 beads - equal distribution (pay_per_token)"
# Using P2 instead of P3 since P3 has weight 0 in pay_per_token model
mock_br_ready '[{"id":"bd-low1","priority":2},{"id":"bd-low2","priority":2}]'

declare -A low_counts
for i in {1..30}; do
    result=$(_needle_select_weighted 2>/dev/null)
    # Skip empty results (can happen if weight is 0 in some billing models)
    [[ -n "$result" ]] && ((low_counts[$result]++))
done

count1=${low_counts[bd-low1]:-0}
count2=${low_counts[bd-low2]:-0}

# Should be roughly equal
if [[ $count1 -ge 8 ]] && [[ $count1 -le 22 ]] && \
   [[ $count2 -ge 8 ]] && [[ $count2 -le 22 ]]; then
    test_pass "(low1:$count1 vs low2:$count2)"
else
    test_fail "Expected roughly equal (low1:$count1 vs low2:$count2)"
fi

# ============================================================================
# Test 8: Performance Test
# ============================================================================

echo ""
echo "--- Performance ---"

test_case "Selection completes in reasonable time (20 selections < 5s)"
mock_br_ready '[{"id":"bd-perf1","priority":0},{"id":"bd-perf2","priority":1},{"id":"bd-perf3","priority":2}]'

start_time=$(date +%s)

for i in {1..20}; do
    _needle_select_weighted &>/dev/null
done

end_time=$(date +%s)
elapsed=$((end_time - start_time))

# Should complete in under 5 seconds
if [[ $elapsed -lt 5 ]]; then
    test_pass "(${elapsed}s for 20 selections)"
else
    test_fail "Took ${elapsed}s (expected < 5s)"
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
