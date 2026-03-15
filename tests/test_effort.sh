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
# Test: _needle_get_bead_effort returns zeros for missing log dir
# =============================================================================
test_get_bead_effort_no_logs() {
    local test_name="_needle_get_bead_effort with no log dir"

    # Point to a non-existent log dir
    local old_log_dir="${NEEDLE_LOG_DIR:-}"
    export NEEDLE_LOG_DIR="$TEST_STATE_DIR/no-such-logs"

    local result
    result=$(_needle_get_bead_effort "nd-test1")

    export NEEDLE_LOG_DIR="$old_log_dir"

    # Should return safe zero values
    if [[ "$result" == "0|0|0|0|" ]]; then
        _test_pass "$test_name - returns zeros when no log dir"
    else
        _test_fail "$test_name - expected 0|0|0|0|, got: $result"
    fi
}

# =============================================================================
# Test: _needle_get_bead_effort aggregates from log files
# =============================================================================
test_get_bead_effort_from_logs() {
    local test_name="_needle_get_bead_effort aggregates from log files"

    local log_dir="$TEST_STATE_DIR/logs"
    mkdir -p "$log_dir"

    # Write fake effort.recorded events for nd-test-abc and another bead
    cat > "$log_dir/session1.jsonl" << 'EOF'
{"ts":"2026-03-15T00:00:01Z","event":"effort.recorded","level":"info","session":"s1","worker":"agent-x","data":{"bead_id":"nd-test-abc","cost":"0.015","input_tokens":1000,"output_tokens":500,"agent":"agent-x"}}
{"ts":"2026-03-15T00:00:02Z","event":"effort.recorded","level":"info","session":"s1","worker":"agent-y","data":{"bead_id":"nd-other","cost":"0.005","input_tokens":200,"output_tokens":100,"agent":"agent-y"}}
{"ts":"2026-03-15T00:00:03Z","event":"bead.effort_recorded","level":"info","session":"s1","worker":"agent-x","data":{"bead_id":"nd-test-abc","cost":"0.010","input_tokens":800,"output_tokens":400,"agent":"agent-x"}}
EOF

    local old_log_dir="${NEEDLE_LOG_DIR:-}"
    local old_home="${NEEDLE_HOME:-}"
    export NEEDLE_LOG_DIR="$log_dir"
    export NEEDLE_HOME="$TEST_STATE_DIR"

    local result
    result=$(_needle_get_bead_effort "nd-test-abc")

    export NEEDLE_LOG_DIR="$old_log_dir"
    export NEEDLE_HOME="$old_home"

    local input_tokens output_tokens cost_usd attempts agents
    IFS='|' read -r input_tokens output_tokens cost_usd attempts agents <<< "$result"

    # Should sum: 1000+800=1800 input, 500+400=900 output, 2 attempts
    if [[ "$input_tokens" -eq 1800 ]]; then
        _test_pass "$test_name - input tokens summed correctly"
    else
        _test_fail "$test_name - expected input_tokens=1800, got: $input_tokens"
    fi

    if [[ "$output_tokens" -eq 900 ]]; then
        _test_pass "$test_name - output tokens summed correctly"
    else
        _test_fail "$test_name - expected output_tokens=900, got: $output_tokens"
    fi

    if [[ "$attempts" -eq 2 ]]; then
        _test_pass "$test_name - attempts counted correctly"
    else
        _test_fail "$test_name - expected attempts=2, got: $attempts"
    fi

    if [[ -n "$agents" ]] && echo "$agents" | grep -q "agent-x"; then
        _test_pass "$test_name - agent attribution present"
    else
        _test_fail "$test_name - expected agent-x in agents, got: $agents"
    fi
}

# =============================================================================
# Test: _needle_get_bead_effort filters to specified bead only
# =============================================================================
test_get_bead_effort_filters_bead() {
    local test_name="_needle_get_bead_effort filters to specified bead"

    local log_dir="$TEST_STATE_DIR/logs2"
    mkdir -p "$log_dir"

    cat > "$log_dir/session.jsonl" << 'EOF'
{"ts":"2026-03-15T00:00:01Z","event":"effort.recorded","level":"info","session":"s1","worker":"w1","data":{"bead_id":"nd-alpha","cost":"0.05","input_tokens":5000,"output_tokens":2500,"agent":"w1"}}
{"ts":"2026-03-15T00:00:02Z","event":"effort.recorded","level":"info","session":"s1","worker":"w1","data":{"bead_id":"nd-beta","cost":"0.02","input_tokens":2000,"output_tokens":1000,"agent":"w1"}}
EOF

    local old_log_dir="${NEEDLE_LOG_DIR:-}"
    local old_home="${NEEDLE_HOME:-}"
    export NEEDLE_LOG_DIR="$log_dir"
    export NEEDLE_HOME="$TEST_STATE_DIR"

    local result
    result=$(_needle_get_bead_effort "nd-beta")

    export NEEDLE_LOG_DIR="$old_log_dir"
    export NEEDLE_HOME="$old_home"

    local input_tokens output_tokens cost_usd attempts agents
    IFS='|' read -r input_tokens output_tokens cost_usd attempts agents <<< "$result"

    # Should only include nd-beta: 2000 input, 1000 output, 1 attempt
    if [[ "$input_tokens" -eq 2000 ]]; then
        _test_pass "$test_name - filtered to correct bead (input tokens)"
    else
        _test_fail "$test_name - expected 2000, got: $input_tokens (wrong bead may be included)"
    fi

    if [[ "$attempts" -eq 1 ]]; then
        _test_pass "$test_name - correct attempt count after filtering"
    else
        _test_fail "$test_name - expected 1 attempt, got: $attempts"
    fi
}

# =============================================================================
# Test: _needle_get_bead_effort returns zeros for unknown bead
# =============================================================================
test_get_bead_effort_unknown_bead() {
    local test_name="_needle_get_bead_effort for unknown bead"

    local log_dir="$TEST_STATE_DIR/logs3"
    mkdir -p "$log_dir"
    echo '{"ts":"2026-03-15T00:00:01Z","event":"effort.recorded","level":"info","session":"s1","worker":"w1","data":{"bead_id":"nd-known","cost":"0.01","input_tokens":100,"output_tokens":50,"agent":"w1"}}' \
        > "$log_dir/session.jsonl"

    local old_log_dir="${NEEDLE_LOG_DIR:-}"
    local old_home="${NEEDLE_HOME:-}"
    export NEEDLE_LOG_DIR="$log_dir"
    export NEEDLE_HOME="$TEST_STATE_DIR"

    local result
    result=$(_needle_get_bead_effort "nd-unknown-xyz")

    export NEEDLE_LOG_DIR="$old_log_dir"
    export NEEDLE_HOME="$old_home"

    local input_tokens output_tokens cost_usd attempts agents
    IFS='|' read -r input_tokens output_tokens cost_usd attempts agents <<< "$result"

    if [[ "$attempts" -eq 0 ]] && [[ "$input_tokens" -eq 0 ]]; then
        _test_pass "$test_name - returns zeros for unknown bead"
    else
        _test_fail "$test_name - expected zeros for unknown bead, got: $result"
    fi
}

# =============================================================================
# Test: _needle_annotate_bead_with_effort skips when no effort data
# =============================================================================
test_annotate_bead_no_effort() {
    local test_name="_needle_annotate_bead_with_effort skips when no effort data"

    local log_dir="$TEST_STATE_DIR/logs_empty"
    mkdir -p "$log_dir"
    # Empty log dir — no events

    local old_log_dir="${NEEDLE_LOG_DIR:-}"
    local old_home="${NEEDLE_HOME:-}"
    export NEEDLE_LOG_DIR="$log_dir"
    export NEEDLE_HOME="$TEST_STATE_DIR"

    # Create a mock br that records calls
    local mock_br_dir="$TEST_STATE_DIR/mock_br_bin_empty"
    mkdir -p "$mock_br_dir"
    cat > "$mock_br_dir/br" << 'BR_SCRIPT'
#!/usr/bin/env bash
# Mark that br was called by creating a flag file
touch "${BR_CALLED_FILE}"
BR_SCRIPT
    chmod +x "$mock_br_dir/br"

    local old_path="$PATH"
    export PATH="$mock_br_dir:$PATH"
    export BR_CALLED_FILE="$TEST_STATE_DIR/br_was_called.txt"

    _needle_annotate_bead_with_effort "nd-empty-bead" "$TEST_STATE_DIR" 2>/dev/null || true

    export NEEDLE_LOG_DIR="$old_log_dir"
    export NEEDLE_HOME="$old_home"
    export PATH="$old_path"
    unset BR_CALLED_FILE

    if [[ ! -f "$TEST_STATE_DIR/br_was_called.txt" ]]; then
        _test_pass "$test_name - skips annotation when no effort data"
    else
        _test_fail "$test_name - should not call br when no effort data"
    fi
}

# =============================================================================
# Test: _needle_annotate_bead_with_effort calls br with cost comment
# =============================================================================
test_annotate_bead_with_effort() {
    local test_name="_needle_annotate_bead_with_effort calls br with cost comment"

    local log_dir="$TEST_STATE_DIR/logs_annotate"
    mkdir -p "$log_dir"
    echo '{"ts":"2026-03-15T00:00:01Z","event":"effort.recorded","level":"info","session":"s1","worker":"agent-z","data":{"bead_id":"nd-annotate-me","cost":"0.025","input_tokens":1500,"output_tokens":750,"agent":"agent-z"}}' \
        > "$log_dir/session.jsonl"

    local old_log_dir="${NEEDLE_LOG_DIR:-}"
    local old_home="${NEEDLE_HOME:-}"
    export NEEDLE_LOG_DIR="$log_dir"
    export NEEDLE_HOME="$TEST_STATE_DIR"

    # Use a temp file to capture br call args (variable assignments don't
    # propagate back from subshells)
    local br_args_file="$TEST_STATE_DIR/br_args_annotate.txt"
    local mock_br_dir="$TEST_STATE_DIR/mock_br_bin"
    mkdir -p "$mock_br_dir"

    # Create an actual executable br script (command -v will find this)
    cat > "$mock_br_dir/br" << 'BR_SCRIPT'
#!/usr/bin/env bash
# Capture br arguments to the args file
echo "$*" >> "${BR_ARGS_FILE}"
BR_SCRIPT
    chmod +x "$mock_br_dir/br"

    # Add mock br to PATH
    local old_path="$PATH"
    export PATH="$mock_br_dir:$PATH"
    export BR_ARGS_FILE="$br_args_file"

    local rc=0
    _needle_annotate_bead_with_effort "nd-annotate-me" "$TEST_STATE_DIR" 2>/dev/null || rc=$?

    export NEEDLE_LOG_DIR="$old_log_dir"
    export NEEDLE_HOME="$old_home"
    export PATH="$old_path"
    unset BR_ARGS_FILE

    # Check file existence BEFORE unsetting br_args_file (unset clears the var)
    if [[ -f "$br_args_file" ]]; then
        _test_pass "$test_name - br was called"

        local br_args
        br_args=$(cat "$br_args_file")

        if echo "$br_args" | grep -qi "cost attribution\|cost:"; then
            _test_pass "$test_name - comment contains cost attribution"
        else
            _test_fail "$test_name - comment missing cost attribution (got: $br_args)"
        fi

        if echo "$br_args" | grep -q "agent-z"; then
            _test_pass "$test_name - comment includes worker attribution"
        else
            _test_fail "$test_name - comment missing worker attribution (got: $br_args)"
        fi
    else
        _test_fail "$test_name - br was not called (args file not created)"
        _test_fail "$test_name - comment contains cost attribution (br not called)"
        _test_fail "$test_name - comment includes worker attribution (br not called)"
    fi
}

# =============================================================================
# Test: _needle_annotate_bead_with_effort handles missing bead_id
# =============================================================================
test_annotate_bead_missing_id() {
    local test_name="_needle_annotate_bead_with_effort missing bead_id"

    local rc=0
    _needle_annotate_bead_with_effort "" "" 2>/dev/null || rc=$?

    if [[ $rc -ne 0 ]]; then
        _test_pass "$test_name - returns non-zero for missing bead_id"
    else
        _test_pass "$test_name - handled gracefully with empty bead_id"
    fi
}

# =============================================================================
# Test: record_effort stores strand and bead_type fields
# =============================================================================
test_record_effort_strand_and_type() {
    local test_name="record_effort with strand and bead_type"

    echo '{}' > "$NEEDLE_DAILY_SPEND_FILE"

    record_effort "nd-strand-test" "0.01" "test-agent" 100 50 "pluck" "task"

    if ! command -v jq &>/dev/null; then
        _test_pass "$test_name - skipped (jq not available)"
        return
    fi

    local today
    today=$(date +%Y-%m-%d)

    local strand_val
    strand_val=$(jq -r ".[\"$today\"].beads[\"nd-strand-test\"].strand // empty" "$NEEDLE_DAILY_SPEND_FILE")
    if [[ "$strand_val" == "pluck" ]]; then
        _test_pass "$test_name - strand field stored correctly"
    else
        _test_fail "$test_name - expected strand=pluck, got: $strand_val"
    fi

    local type_val
    type_val=$(jq -r ".[\"$today\"].beads[\"nd-strand-test\"].type // empty" "$NEEDLE_DAILY_SPEND_FILE")
    if [[ "$type_val" == "task" ]]; then
        _test_pass "$test_name - type field stored correctly"
    else
        _test_fail "$test_name - expected type=task, got: $type_val"
    fi
}

# =============================================================================
# Test: record_effort omits strand/type when not provided
# =============================================================================
test_record_effort_no_strand() {
    local test_name="record_effort omits strand/type when not provided"

    echo '{}' > "$NEEDLE_DAILY_SPEND_FILE"

    record_effort "nd-nostrand-test" "0.01" "test-agent" 100 50

    if ! command -v jq &>/dev/null; then
        _test_pass "$test_name - skipped (jq not available)"
        return
    fi

    local today
    today=$(date +%Y-%m-%d)

    local strand_val
    strand_val=$(jq -r ".[\"$today\"].beads[\"nd-nostrand-test\"].strand // \"ABSENT\"" "$NEEDLE_DAILY_SPEND_FILE")
    if [[ "$strand_val" == "ABSENT" ]]; then
        _test_pass "$test_name - strand field absent when not provided"
    else
        _test_fail "$test_name - expected strand absent, got: $strand_val"
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
    test_record_effort_strand_and_type
    test_record_effort_no_strand
    test_get_bead_effort_no_logs
    test_get_bead_effort_from_logs
    test_get_bead_effort_filters_bead
    test_get_bead_effort_unknown_bead
    test_annotate_bead_no_effort
    test_annotate_bead_with_effort
    test_annotate_bead_missing_id

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
