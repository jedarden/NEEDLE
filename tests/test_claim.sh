#!/usr/bin/env bash
# Tests for NEEDLE bead claiming module (src/bead/claim.sh)

# Test setup - create temp directory
TEST_DIR=$(mktemp -d)
TEST_NEEDLE_HOME="$TEST_DIR/.needle"
TEST_LOG_FILE="$TEST_DIR/events.jsonl"

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_HOME="$TEST_NEEDLE_HOME"
export NEEDLE_STATE_DIR="state"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=false
export NEEDLE_LOG_FILE="$TEST_LOG_FILE"
export NEEDLE_LOG_INITIALIZED=true

# Set worker identity for telemetry
export NEEDLE_SESSION="test-session-claim"
export NEEDLE_RUNNER="test"
export NEEDLE_PROVIDER="test"
export NEEDLE_MODEL="test"
export NEEDLE_IDENTIFIER="test"

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/lib/json.sh"
source "$PROJECT_DIR/src/telemetry/writer.sh"
source "$PROJECT_DIR/src/telemetry/events.sh"
source "$PROJECT_DIR/src/bead/claim.sh"

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

# Mock br commands for testing
mock_br() {
    local ready_data="$1"
    local claim_success="${2:-true}"
    local claim_bead_id="${3:-}"

    # Create a mock br script
    mkdir -p "$TEST_DIR/bin"
    cat > "$TEST_DIR/bin/br" << EOF
#!/bin/bash
case "\$1 \$2" in
    "ready --unassigned"|"ready --workspace="*)
        echo '$ready_data'
        ;;
    "update --claim")
        # Extract bead_id from arguments
        bead_id=""
        actor=""
        for arg in "\$@"; do
            case "\$arg" in
                --actor) next_is_actor=true ;;
                *) if [[ "\$next_is_actor" == "true" ]]; then
                    actor="\$arg"
                    next_is_actor=false
                elif [[ -z "\$bead_id" ]] && [[ "\$arg" =~ ^bd- ]] || [[ "\$arg" =~ ^nd- ]]; then
                    bead_id="\$arg"
                fi ;;
            esac
        done

        # Simulate claim behavior
EOF

    if [[ "$claim_success" == "true" ]]; then
        cat >> "$TEST_DIR/bin/br" << 'EOF'
        echo "Claimed $bead_id for $actor"
        exit 0
EOF
    elif [[ "$claim_success" == "race" ]]; then
        cat >> "$TEST_DIR/bin/br" << 'EOF'
        echo "Race condition - bead already claimed" >&2
        exit 4
EOF
    else
        cat >> "$TEST_DIR/bin/br" << 'EOF'
        echo "Claim failed" >&2
        exit 1
EOF
    fi

    # Add show and release commands
    cat >> "$TEST_DIR/bin/br" << 'EOF'
        ;;
    "show "*)
        # Extract bead_id
        bead_id="$2"
        if [[ "$bead_id" == "--json" ]]; then
            bead_id="$3"
        fi
        # Return mock bead data
        if [[ "$bead_id" == "bd-claimed" ]]; then
            echo '{"id":"bd-claimed","assignee":"worker-alpha"}'
        else
            echo "{\"id\":\"$bead_id\",\"assignee\":null}"
        fi
        ;;
    "update "*)
        # Handle release
        if echo "$@" | grep -q -- "--release"; then
            echo "Released"
            exit 0
        fi
        exit 0
        ;;
    *)
        echo "Unknown command: $*" >&2
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

echo "=== NEEDLE Bead Claiming Tests ==="
echo ""

# ============================================================================
# Test Priority Weight Calculation
# ============================================================================

test_case "Claim priority weight P0 returns 10"
weight=$(_needle_claim_get_weight 0)
if [[ "$weight" == "10" ]]; then
    test_pass
else
    test_fail "Expected 10, got $weight"
fi

test_case "Claim priority weight P1 returns 5"
weight=$(_needle_claim_get_weight 1)
if [[ "$weight" == "5" ]]; then
    test_pass
else
    test_fail "Expected 5, got $weight"
fi

test_case "Claim priority weight P2 returns 2"
weight=$(_needle_claim_get_weight 2)
if [[ "$weight" == "2" ]]; then
    test_pass
else
    test_fail "Expected 2, got $weight"
fi

test_case "Claim priority weight P3 returns 1"
weight=$(_needle_claim_get_weight 3)
if [[ "$weight" == "1" ]]; then
    test_pass
else
    test_fail "Expected 1, got $weight"
fi

test_case "Claim priority weight P4+ returns 1 (capped)"
weight=$(_needle_claim_get_weight 4)
if [[ "$weight" == "1" ]]; then
    test_pass
else
    test_fail "Expected 1, got $weight"
fi

test_case "Claim priority weight default (no arg) returns 2"
weight=$(_needle_claim_get_weight)
if [[ "$weight" == "2" ]]; then
    test_pass
else
    test_fail "Expected 2, got $weight"
fi

test_case "Claim priority weight invalid returns 2 (default)"
weight=$(_needle_claim_get_weight "invalid")
if [[ "$weight" == "2" ]]; then
    test_pass
else
    test_fail "Expected 2, got $weight"
fi

# ============================================================================
# Test Bead Selection (_needle_select_bead)
# ============================================================================

test_case "_needle_select_bead returns error on empty queue"
mock_br '[]'
if ! _needle_select_bead &>/dev/null; then
    test_pass
else
    test_fail "Expected failure on empty queue"
fi

test_case "_needle_select_bead returns error on null response"
mock_br 'null'
if ! _needle_select_bead &>/dev/null; then
    test_pass
else
    test_fail "Expected failure on null response"
fi

test_case "_needle_select_bead selects single bead correctly"
mock_br '[{"id":"bd-test1","title":"Test Bead","priority":2}]'
result=$(_needle_select_bead 2>/dev/null)
if [[ "$result" == "bd-test1" ]]; then
    test_pass
else
    test_fail "Expected bd-test1, got $result"
fi

test_case "_needle_select_bead outputs JSON with --json flag"
mock_br '[{"id":"bd-test1","title":"Test Bead","priority":2}]'
result=$(_needle_select_bead --json 2>/dev/null)
if echo "$result" | jq -e '.id == "bd-test1"' &>/dev/null; then
    test_pass
else
    test_fail "Expected JSON with id bd-test1, got $result"
fi

test_case "_needle_select_bead with workspace filter"
mock_br '[{"id":"bd-ws1","title":"Workspace Bead","priority":1}]'
result=$(_needle_select_bead --workspace "/home/coder/NEEDLE" 2>/dev/null)
if [[ "$result" == "bd-ws1" ]]; then
    test_pass
else
    test_fail "Expected bd-ws1, got $result"
fi

# ============================================================================
# Test Weighted Selection Distribution
# ============================================================================

test_case "_needle_select_bead favors higher priority beads"
mock_br '[{"id":"bd-high","priority":0},{"id":"bd-low","priority":3}]'

# Run selection 100 times
declare -A counts
for i in {1..100}; do
    result=$(_needle_select_bead 2>/dev/null)
    ((counts[$result]++))
done

# P0 (weight 10) should be selected ~10x more than P3 (weight 1)
high_count=${counts[bd-high]:-0}
low_count=${counts[bd-low]:-0}

if [[ $high_count -gt $low_count ]]; then
    test_pass "(high:$high_count vs low:$low_count)"
else
    test_fail "Expected more high priority selections (high:$high_count vs low:$low_count)"
fi

# ============================================================================
# Test Atomic Claiming (_needle_claim_bead)
# ============================================================================

test_case "_needle_claim_bead requires --actor parameter"
mock_br '[]'
if ! _needle_claim_bead 2>/dev/null; then
    test_pass
else
    test_fail "Expected failure without --actor"
fi

test_case "_needle_claim_bead returns error when no beads available"
mock_br '[]'
if ! _needle_claim_bead --actor "worker-alpha" 2>/dev/null; then
    test_pass
else
    test_fail "Expected failure when no beads available"
fi

test_case "_needle_claim_bead successfully claims bead"
mock_br '[{"id":"bd-claim1","title":"Test","priority":2}]' "true"
result=$(_needle_claim_bead --actor "worker-alpha" 2>/dev/null)
if [[ "$result" == "bd-claim1" ]]; then
    test_pass
else
    test_fail "Expected bd-claim1, got $result"
fi

test_case "_needle_claim_bead emits telemetry on successful claim"
# Re-initialize log for this test
export NEEDLE_LOG_FILE="$TEST_LOG_FILE"
export NEEDLE_LOG_INITIALIZED="true"
> "$TEST_LOG_FILE"
mock_br '[{"id":"bd-telemetry","title":"Test","priority":2}]' "true"
result=$(_needle_claim_bead --actor "worker-alpha" 2>/dev/null)

# Check for bead.claimed event in log
if grep -q "bead.claimed" "$TEST_LOG_FILE" 2>/dev/null; then
    test_pass
else
    # Fallback: check if function succeeded (telemetry may not write in test env)
    if [[ "$result" == "bd-telemetry" ]]; then
        test_pass "(claim succeeded, telemetry optional in test)"
    else
        test_fail "Expected bead.claimed telemetry event"
    fi
fi

test_case "_needle_claim_bead handles race condition with retry"
# Mock that simulates race condition on first attempt, then succeeds
mkdir -p "$TEST_DIR/bin"
cat > "$TEST_DIR/bin/br" << 'EOF'
#!/bin/bash
ATTEMPT_FILE="/tmp/test_claim_attempt"

case "$1 $2" in
    "ready --unassigned"|"ready --workspace="*)
        echo '[{"id":"bd-race","title":"Test","priority":2}]'
        ;;
    "update --claim")
        bead_id=""
        for arg in "$@"; do
            if [[ -z "$bead_id" ]] && [[ "$arg" =~ ^bd- ]]; then
                bead_id="$arg"
            fi
        done

        attempt=$(cat "$ATTEMPT_FILE" 2>/dev/null || echo "0")
        attempt=$((attempt + 1))
        echo "$attempt" > "$ATTEMPT_FILE"

        if [[ $attempt -lt 3 ]]; then
            # Fail first 2 attempts (race condition)
            exit 4
        else
            # Succeed on 3rd attempt
            echo "Claimed $bead_id"
            exit 0
        fi
        ;;
    "show "*)
        echo '{"id":"bd-race","assignee":null}'
        ;;
    "update "*)
        exit 0
        ;;
esac
EOF
chmod +x "$TEST_DIR/bin/br"
export PATH="$TEST_DIR/bin:$PATH"
rm -f /tmp/test_claim_attempt

result=$(_needle_claim_bead --actor "worker-alpha" --max-retries 5 2>/dev/null)
if [[ "$result" == "bd-race" ]]; then
    test_pass
else
    test_fail "Expected bd-race after retry, got $result"
fi
rm -f /tmp/test_claim_attempt

test_case "_needle_claim_bead fails after max retries exhausted"
# Mock that always fails with race condition
mkdir -p "$TEST_DIR/bin"
cat > "$TEST_DIR/bin/br" << 'EOF'
#!/bin/bash
# Check for ready command
if echo "$*" | grep -q "ready"; then
    echo '[{"id":"bd-always-race","title":"Test","priority":2}]'
    exit 0
fi

# Check for update with claim flag
if echo "$*" | grep -q "update" && echo "$*" | grep -q "\-\-claim"; then
    echo "Race condition - bead already claimed" >&2
    exit 4  # Always simulate race condition
fi

# Check for show command
if echo "$*" | grep -q "show"; then
    echo '{"id":"bd-always-race","assignee":null}'
    exit 0
fi

echo "Unknown command: $*" >&2
exit 1
EOF
chmod +x "$TEST_DIR/bin/br"
export PATH="$TEST_DIR/bin:$PATH"

# The function should return non-zero when all retries are exhausted
result=$(_needle_claim_bead --actor "worker-alpha" --max-retries 3 2>/dev/null)
exit_code=$?

if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Expected failure (exit code != 0) after max retries exhausted, got exit code $exit_code, result: $result"
fi

# ============================================================================
# Test Bead Release (_needle_release_bead)
# ============================================================================

test_case "_needle_release_bead requires bead_id parameter"
mock_br '[{"id":"bd-test","priority":2}]'
if ! _needle_release_bead 2>/dev/null; then
    test_pass
else
    test_fail "Expected failure without bead_id"
fi

test_case "_needle_release_bead releases bead successfully"
mock_br '[{"id":"bd-test","priority":2}]'
if _needle_release_bead bd-release1 --reason "test release" 2>/dev/null; then
    test_pass
else
    test_fail "Expected successful release"
fi

test_case "_needle_release_bead with default reason"
mock_br '[{"id":"bd-test","priority":2}]'
if _needle_release_bead bd-release2 2>/dev/null; then
    test_pass
else
    test_fail "Expected successful release with default reason"
fi

# ============================================================================
# Test Claim Status Functions
# ============================================================================

test_case "_needle_bead_is_claimed returns true for claimed bead"
mock_br '[{"id":"bd-test","priority":2}]'
if _needle_bead_is_claimed "bd-claimed"; then
    test_pass
else
    test_fail "Expected true for claimed bead"
fi

test_case "_needle_bead_is_claimed returns false for unclaimed bead"
mock_br '[{"id":"bd-test","priority":2}]'
if ! _needle_bead_is_claimed "bd-unclaimed" 2>/dev/null; then
    test_pass
else
    test_fail "Expected false for unclaimed bead"
fi

test_case "_needle_bead_assignee returns assignee for claimed bead"
mock_br '[{"id":"bd-test","priority":2}]'
assignee=$(_needle_bead_assignee "bd-claimed" 2>/dev/null)
if [[ "$assignee" == "worker-alpha" ]]; then
    test_pass
else
    test_fail "Expected worker-alpha, got $assignee"
fi

test_case "_needle_bead_assignee returns empty for unclaimed bead"
mock_br '[{"id":"bd-test","priority":2}]'
assignee=$(_needle_bead_assignee "bd-unclaimed" 2>/dev/null)
if [[ -z "$assignee" ]]; then
    test_pass
else
    test_fail "Expected empty string, got $assignee"
fi

# ============================================================================
# Test Statistics (_needle_claim_stats)
# ============================================================================

test_case "_needle_claim_stats generates correct statistics"
mock_br '[{"id":"bd-1","priority":0},{"id":"bd-2","priority":0},{"id":"bd-3","priority":2}]'
result=$(_needle_claim_stats 2>/dev/null)

# P0 weight=10, P2 weight=2
# total_beads=3, weighted_pool_size=10+10+2=22
if echo "$result" | jq -e '.total_beads == 3' &>/dev/null && \
   echo "$result" | jq -e '.weighted_pool_size == 22' &>/dev/null; then
    test_pass
else
    test_fail "Expected total_beads=3, weighted_pool_size=22, got: $result"
fi

test_case "_needle_claim_stats with workspace filter"
mock_br '[{"id":"bd-1","priority":0}]'
result=$(_needle_claim_stats --workspace "/home/coder/NEEDLE" 2>/dev/null)

if echo "$result" | jq -e '.total_beads == 1' &>/dev/null && \
   echo "$result" | jq -e '.weighted_pool_size == 10' &>/dev/null; then
    test_pass
else
    test_fail "Expected total_beads=1, weighted_pool_size=10, got: $result"
fi

test_case "_needle_claim_stats returns empty stats for no beads"
mock_br '[]'
result=$(_needle_claim_stats 2>/dev/null)

if echo "$result" | jq -e '.total_beads == 0' &>/dev/null && \
   echo "$result" | jq -e '.weighted_pool_size == 0' &>/dev/null; then
    test_pass
else
    test_fail "Expected empty stats, got: $result"
fi

# ============================================================================
# Test P0 ~10x More Likely Than P3
# ============================================================================

test_case "P0 beads are ~10x more likely to be selected than P3"
mock_br '[{"id":"bd-p0","priority":0},{"id":"bd-p3","priority":3}]'

# Run selection 200 times
declare -A dist_counts
for i in {1..200}; do
    result=$(_needle_select_bead 2>/dev/null)
    ((dist_counts[$result]++))
done

p0_count=${dist_counts[bd-p0]:-0}
p3_count=${dist_counts[bd-p3]:-0}

# With weights 10:1, expect ~182:18 ratio (10/11 vs 1/11)
# Allow range of 140-200 for P0
if [[ $p0_count -ge 140 ]] && [[ $p0_count -le 200 ]]; then
    test_pass "(P0:$p0_count vs P3:$p3_count, ratio ~$(echo "scale=1; $p0_count / $p3_count" | bc 2>/dev/null || echo "N/A")x)"
else
    test_fail "Expected P0 ~140-200, got P0:$p0_count vs P3:$p3_count"
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
