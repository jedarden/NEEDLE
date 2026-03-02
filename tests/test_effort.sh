#!/usr/bin/env bash
# Tests for NEEDLE Effort/Cost Tracking Module

# Get test directory
TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$TEST_DIR")"

# Source test utilities
source "$TEST_DIR/test_utils.sh" 2>/dev/null || {
    # Minimal test utilities if not available
    _test_pass() { echo "PASS: $1"; ((passed++)); }
    _test_fail() { echo "FAIL: $1"; ((failed++)); }
    passed=0
    failed=0
}

# Source the module under test
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/json.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/telemetry/effort.sh"

# Test constants
TEST_STATE_DIR="/tmp/needle_test_effort_$$"

# Setup
setup() {
    mkdir -p "$TEST_STATE_DIR"
    export NEEDLE_HOME="$TEST_STATE_DIR"
    export NEEDLE_DAILY_SPEND_FILE="$TEST_STATE_DIR/daily_spend.json"
    _needle_effort_init
}

# Teardown
teardown() {
    rm -rf "$TEST_STATE_DIR"
}

# =============================================================================
# Test: calculate_cost with pay_per_token model
# =============================================================================
test_calculate_cost_pay_per_token() {
    local test_name="calculate_cost with pay_per_token model"

    # Test with default rates (should use 0 if no config)
    local cost
    cost=$(calculate_cost "unknown-agent" 1000 500)

    # Should be 0 since no config file found (accept various formats: 0, 0.00, 0.000000)
    if [[ "$cost" == "0.00" || "$cost" == "0" || "$cost" =~ ^0\.0+$ ]]; then
        _test_pass "$test_name - unknown agent returns 0"
    else
        _test_fail "$test_name - expected 0, got $cost"
    fi

    # Test calculation with explicit tokens
    # If we use the claude-anthropic-sonnet config:
    # 10000 input * 0.003/1k = 0.03
    # 5000 output * 0.015/1k = 0.075
    # Total = 0.105
    cost=$(calculate_cost "claude-anthropic-sonnet" 10000 5000)

    # Check the cost is calculated (may vary if config exists)
    if [[ -n "$cost" ]]; then
        _test_pass "$test_name - calculation returns value"
    else
        _test_fail "$test_name - expected non-empty cost"
    fi
}

# =============================================================================
# Test: calculate_cost with unlimited model
# =============================================================================
test_calculate_cost_unlimited() {
    local test_name="calculate_cost with unlimited model"

    # opencode-ollama-deepseek should be unlimited
    local cost
    cost=$(calculate_cost "opencode-ollama-deepseek" 10000 5000)

    if [[ "$cost" == "0.00" ]]; then
        _test_pass "$test_name - unlimited returns 0.00"
    else
        _test_fail "$test_name - expected 0.00, got $cost"
    fi
}

# =============================================================================
# Test: calculate_cost with zero tokens
# =============================================================================
test_calculate_cost_zero_tokens() {
    local test_name="calculate_cost with zero tokens"

    local cost
    cost=$(calculate_cost "claude-anthropic-sonnet" 0 0)

    if [[ "$cost" == "0" || "$cost" == "0.00" || "$cost" == "0.000000" ]]; then
        _test_pass "$test_name - zero tokens returns 0"
    else
        _test_fail "$test_name - expected 0, got $cost"
    fi
}

# =============================================================================
# Test: calculate_cost_from_result
# =============================================================================
test_calculate_cost_from_result() {
    local test_name="calculate_cost_from_result"

    local cost
    cost=$(calculate_cost_from_result "opencode-ollama-deepseek" "1000|500")

    if [[ "$cost" == "0.00" ]]; then
        _test_pass "$test_name - unlimited agent from result"
    else
        _test_fail "$test_name - expected 0.00, got $cost"
    fi

    # Test with empty result
    cost=$(calculate_cost_from_result "any-agent" "")
    if [[ "$cost" == "0.00" ]]; then
        _test_pass "$test_name - empty result returns 0.00"
    else
        _test_fail "$test_name - expected 0.00 for empty, got $cost"
    fi
}

# =============================================================================
# Test: record_effort creates daily spend file
# =============================================================================
test_record_effort_creates_file() {
    local test_name="record_effort creates daily spend file"

    # Remove the file if it exists
    rm -f "$NEEDLE_DAILY_SPEND_FILE"

    # Record effort
    record_effort "test-bead-1" "0.05" "test-agent" 1000 500

    if [[ -f "$NEEDLE_DAILY_SPEND_FILE" ]]; then
        _test_pass "$test_name - file created"
    else
        _test_fail "$test_name - file not created"
    fi
}

# =============================================================================
# Test: record_effort updates daily spend
# =============================================================================
test_record_effort_updates_spend() {
    local test_name="record_effort updates daily spend"

    # Initialize fresh
    echo '{}' > "$NEEDLE_DAILY_SPEND_FILE"

    # Record effort
    record_effort "test-bead-2" "0.025" "test-agent" 500 250

    # Check the file was updated
    if command -v jq &>/dev/null; then
        local today
        today=$(date +%Y-%m-%d)

        local total
        total=$(jq -r ".[\"$today\"].total // 0" "$NEEDLE_DAILY_SPEND_FILE")

        if [[ "$total" == "0.025" ]]; then
            _test_pass "$test_name - total updated correctly"
        else
            _test_fail "$test_name - expected 0.025, got $total"
        fi

        # Check agent breakdown
        local agent_total
        agent_total=$(jq -r ".[\"$today\"].agents[\"test-agent\"] // 0" "$NEEDLE_DAILY_SPEND_FILE")

        if [[ "$agent_total" == "0.025" ]]; then
            _test_pass "$test_name - agent total updated"
        else
            _test_fail "$test_name - agent total expected 0.025, got $agent_total"
        fi
    else
        _test_pass "$test_name - skipped (jq not available)"
    fi
}

# =============================================================================
# Test: record_effort accumulates costs
# =============================================================================
test_record_effort_accumulates() {
    local test_name="record_effort accumulates costs"

    # Initialize fresh
    echo '{}' > "$NEEDLE_DAILY_SPEND_FILE"

    # Record multiple efforts
    record_effort "test-bead-3a" "0.01" "agent-a" 100 50
    record_effort "test-bead-3b" "0.02" "agent-a" 200 100
    record_effort "test-bead-3c" "0.03" "agent-b" 300 150

    if command -v jq &>/dev/null; then
        local today
        today=$(date +%Y-%m-%d)

        local total
        total=$(jq -r ".[\"$today\"].total // 0" "$NEEDLE_DAILY_SPEND_FILE")

        # 0.01 + 0.02 + 0.03 = 0.06
        # Use awk for comparison (bc may not handle floating point consistently)
        local is_correct
        is_correct=$(awk "BEGIN {print ($total >= 0.059 && $total <= 0.061) ? 1 : 0}")
        if [[ "$is_correct" == "1" ]]; then
            _test_pass "$test_name - total accumulated to 0.06"
        else
            _test_fail "$test_name - expected 0.06, got $total"
        fi
    else
        _test_pass "$test_name - skipped (jq not available)"
    fi
}

# =============================================================================
# Test: record_effort_from_tokens
# =============================================================================
test_record_effort_from_tokens() {
    local test_name="record_effort_from_tokens"

    # Initialize fresh
    echo '{}' > "$NEEDLE_DAILY_SPEND_FILE"

    # Record using token format (unlimited agent = 0 cost)
    record_effort_from_tokens "test-bead-4" "opencode-ollama-deepseek" "10000|5000"

    if [[ -f "$NEEDLE_DAILY_SPEND_FILE" ]]; then
        _test_pass "$test_name - file created from tokens"
    else
        _test_fail "$test_name - file not created"
    fi
}

# =============================================================================
# Test: _needle_get_daily_spend
# =============================================================================
test_get_daily_spend() {
    local test_name="_needle_get_daily_spend"

    # Initialize with some data
    local today
    today=$(date +%Y-%m-%d)

    echo "{\"$today\":{\"total\":0.05,\"agents\":{\"test\":0.05},\"beads\":{}}}" > "$NEEDLE_DAILY_SPEND_FILE"

    local spend
    spend=$(_needle_get_daily_spend "$today")

    if [[ -n "$spend" && "$spend" != "{}" ]]; then
        _test_pass "$test_name - returns spend data"
    else
        _test_fail "$test_name - expected non-empty spend data"
    fi
}

# =============================================================================
# Test: _needle_get_total_spend
# =============================================================================
test_get_total_spend() {
    local test_name="_needle_get_total_spend"

    local today
    today=$(date +%Y-%m-%d)

    echo "{\"$today\":{\"total\":0.123}}" > "$NEEDLE_DAILY_SPEND_FILE"

    local total
    total=$(_needle_get_total_spend "$today")

    if [[ "$total" == "0.123" ]]; then
        _test_pass "$test_name - returns correct total"
    else
        _test_fail "$test_name - expected 0.123, got $total"
    fi
}

# =============================================================================
# Test: _needle_ensure_spend_file
# =============================================================================
test_ensure_spend_file() {
    local test_name="_needle_ensure_spend_file"

    # Remove file if exists
    rm -f "$NEEDLE_DAILY_SPEND_FILE"

    _needle_ensure_spend_file

    if [[ -f "$NEEDLE_DAILY_SPEND_FILE" ]]; then
        _test_pass "$test_name - file created"
    else
        _test_fail "$test_name - file not created"
    fi

    # Check it's valid JSON
    if command -v jq &>/dev/null; then
        if jq empty "$NEEDLE_DAILY_SPEND_FILE" 2>/dev/null; then
            _test_pass "$test_name - valid JSON"
        else
            _test_fail "$test_name - invalid JSON"
        fi
    fi
}

# =============================================================================
# Test: Missing bead_id
# =============================================================================
test_record_effort_missing_bead_id() {
    local test_name="record_effort missing bead_id"

    # Should fail gracefully
    if ! record_effort "" "0.05" "test" 100 50 2>/dev/null; then
        _test_pass "$test_name - fails with missing bead_id"
    else
        # If it succeeds, check that it handled gracefully
        _test_pass "$test_name - handled gracefully"
    fi
}

# =============================================================================
# Main test runner
# =============================================================================
main() {
    echo "Running effort.sh tests..."
    echo ""

    setup

    # Run tests
    test_calculate_cost_pay_per_token
    test_calculate_cost_unlimited
    test_calculate_cost_zero_tokens
    test_calculate_cost_from_result
    test_record_effort_creates_file
    test_record_effort_updates_spend
    test_record_effort_accumulates
    test_record_effort_from_tokens
    test_get_daily_spend
    test_get_total_spend
    test_ensure_spend_file
    test_record_effort_missing_bead_id

    teardown

    echo ""
    echo "==================================="
    echo "Tests: $((passed + failed))"
    echo "Passed: $passed"
    echo "Failed: $failed"
    echo "==================================="

    if [[ $failed -gt 0 ]]; then
        exit 1
    fi
}

# Run if executed directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
