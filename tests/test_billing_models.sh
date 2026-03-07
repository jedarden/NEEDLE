#!/usr/bin/env bash
# Tests for NEEDLE Billing Model Profiles Module

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
source "$PROJECT_DIR/src/lib/config.sh"
source "$PROJECT_DIR/src/lib/billing_models.sh"

# Test constants
TEST_STATE_DIR="/tmp/needle_test_billing_$$"

# Setup
setup() {
    mkdir -p "$TEST_STATE_DIR"
    export NEEDLE_HOME="$TEST_STATE_DIR"
    export NEEDLE_CONFIG_FILE="$TEST_STATE_DIR/config.yaml"
}

# Teardown
teardown() {
    rm -rf "$TEST_STATE_DIR"
}

# =============================================================================
# Test: get_billing_model - defaults
# =============================================================================
test_get_billing_model_default() {
    local test_name="get_billing_model_default"

    # Create config without billing section (should default to pay_per_token)
    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
effort:
  budget:
    daily_limit_usd: 50.0
EOF

    clear_config_cache

    local model
    model=$(get_billing_model)

    if [[ "$model" == "pay_per_token" ]]; then
        _test_pass "$test_name - defaults to pay_per_token"
    else
        _test_fail "$test_name - expected pay_per_token, got $model"
    fi
}

# =============================================================================
# Test: get_billing_model - pay_per_token
# =============================================================================
test_get_billing_model_pay_per_token() {
    local test_name="get_billing_model_pay_per_token"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: pay_per_token
  daily_budget_usd: 10.0
EOF

    clear_config_cache

    local model
    model=$(get_billing_model)

    if [[ "$model" == "pay_per_token" ]]; then
        _test_pass "$test_name - returns pay_per_token"
    else
        _test_fail "$test_name - expected pay_per_token, got $model"
    fi
}

# =============================================================================
# Test: get_billing_model - use_or_lose
# =============================================================================
test_get_billing_model_use_or_lose() {
    local test_name="get_billing_model_use_or_lose"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: use_or_lose
  daily_budget_usd: 50.0
EOF

    clear_config_cache

    local model
    model=$(get_billing_model)

    if [[ "$model" == "use_or_lose" ]]; then
        _test_pass "$test_name - returns use_or_lose"
    else
        _test_fail "$test_name - expected use_or_lose, got $model"
    fi
}

# =============================================================================
# Test: get_billing_model - unlimited
# =============================================================================
test_get_billing_model_unlimited() {
    local test_name="get_billing_model_unlimited"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: unlimited
EOF

    clear_config_cache

    local model
    model=$(get_billing_model)

    if [[ "$model" == "unlimited" ]]; then
        _test_pass "$test_name - returns unlimited"
    else
        _test_fail "$test_name - expected unlimited, got $model"
    fi
}

# =============================================================================
# Test: get_billing_budget
# =============================================================================
test_get_billing_budget() {
    local test_name="get_billing_budget"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: pay_per_token
  daily_budget_usd: 10.0
EOF

    clear_config_cache

    local budget
    budget=$(get_billing_budget)

    if [[ "$budget" == "10.0" || "$budget" == "10" ]]; then
        _test_pass "$test_name - returns 10.0"
    else
        _test_fail "$test_name - expected 10.0, got $budget"
    fi
}

# =============================================================================
# Test: _needle_billing_get_enforcement_strategy
# =============================================================================
test_enforcement_strategy_pay_per_token() {
    local test_name="enforcement_strategy_pay_per_token"

    local strategy
    strategy=$(_needle_billing_get_enforcement_strategy "pay_per_token")

    if [[ "$strategy" == "strict" ]]; then
        _test_pass "$test_name - returns strict"
    else
        _test_fail "$test_name - expected strict, got $strategy"
    fi
}

test_enforcement_strategy_use_or_lose() {
    local test_name="enforcement_strategy_use_or_lose"

    local strategy
    strategy=$(_needle_billing_get_enforcement_strategy "use_or_lose")

    if [[ "$strategy" == "target" ]]; then
        _test_pass "$test_name - returns target"
    else
        _test_fail "$test_name - expected target, got $strategy"
    fi
}

test_enforcement_strategy_unlimited() {
    local test_name="enforcement_strategy_unlimited"

    local strategy
    strategy=$(_needle_billing_get_enforcement_strategy "unlimited")

    if [[ "$strategy" == "none" ]]; then
        _test_pass "$test_name - returns none"
    else
        _test_fail "$test_name - expected none, got $strategy"
    fi
}

# =============================================================================
# Test: _needle_billing_get_min_priority
# =============================================================================
test_min_priority_pay_per_token() {
    local test_name="min_priority_pay_per_token"

    local min_priority
    min_priority=$(_needle_billing_get_min_priority "pay_per_token")

    if [[ "$min_priority" == "1" ]]; then
        _test_pass "$test_name - returns 1 (P0-P1)"
    else
        _test_fail "$test_name - expected 1, got $min_priority"
    fi
}

test_min_priority_use_or_lose() {
    local test_name="min_priority_use_or_lose"

    local min_priority
    min_priority=$(_needle_billing_get_min_priority "use_or_lose")

    if [[ "$min_priority" == "2" ]]; then
        _test_pass "$test_name - returns 2 (P0-P2)"
    else
        _test_fail "$test_name - expected 2, got $min_priority"
    fi
}

test_min_priority_unlimited() {
    local test_name="min_priority_unlimited"

    local min_priority
    min_priority=$(_needle_billing_get_min_priority "unlimited")

    if [[ "$min_priority" == "4" ]]; then
        _test_pass "$test_name - returns 4 (all priorities)"
    else
        _test_fail "$test_name - expected 4, got $min_priority"
    fi
}

# =============================================================================
# Test: _needle_billing_get_concurrency
# =============================================================================
test_concurrency_pay_per_token() {
    local test_name="concurrency_pay_per_token"

    # Note: merged config includes defaults which have limits.global_max_concurrent: 20
    # So we expect billing model to respect explicit config value
    # To test billing model defaults, we need to remove from merged config (not feasible)
    # Instead, test that explicit config value is respected
    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: pay_per_token
limits:
  global_max_concurrent: 3
EOF

    clear_config_cache

    local concurrency
    concurrency=$(_needle_billing_get_concurrency "pay_per_token")

    if [[ "$concurrency" == "3" ]]; then
        _test_pass "$test_name - respects explicit config (3)"
    else
        _test_fail "$test_name - expected 3, got $concurrency"
    fi
}

test_concurrency_use_or_lose() {
    local test_name="concurrency_use_or_lose"

    # Test explicit config is respected
    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: use_or_lose
limits:
  global_max_concurrent: 8
EOF

    clear_config_cache

    local concurrency
    concurrency=$(_needle_billing_get_concurrency "use_or_lose")

    if [[ "$concurrency" == "8" ]]; then
        _test_pass "$test_name - respects explicit config (8)"
    else
        _test_fail "$test_name - expected 8, got $concurrency"
    fi
}

test_concurrency_unlimited() {
    local test_name="concurrency_unlimited"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: unlimited
EOF

    clear_config_cache

    local concurrency
    concurrency=$(_needle_billing_get_concurrency "unlimited")

    if [[ "$concurrency" == "20" ]]; then
        _test_pass "$test_name - returns 20"
    else
        _test_fail "$test_name - expected 20, got $concurrency"
    fi
}

# =============================================================================
# Test: _needle_billing_is_strand_enabled - pay_per_token
# =============================================================================
test_strand_enabled_pay_per_token_pluck() {
    local test_name="strand_enabled_pay_per_token_pluck"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: pay_per_token
strands:
  pluck: auto
EOF

    clear_config_cache

    if _needle_billing_is_strand_enabled "pluck" "pay_per_token"; then
        _test_pass "$test_name - pluck enabled in pay_per_token"
    else
        _test_fail "$test_name - pluck should be enabled in pay_per_token"
    fi
}

test_strand_enabled_pay_per_token_weave() {
    local test_name="strand_enabled_pay_per_token_weave"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: pay_per_token
strands:
  weave: auto
EOF

    clear_config_cache

    if _needle_billing_is_strand_enabled "weave" "pay_per_token"; then
        _test_fail "$test_name - weave should be disabled in pay_per_token"
    else
        _test_pass "$test_name - weave disabled in pay_per_token"
    fi
}

# =============================================================================
# Test: _needle_billing_is_strand_enabled - use_or_lose
# =============================================================================
test_strand_enabled_use_or_lose_all() {
    local test_name="strand_enabled_use_or_lose_all"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: use_or_lose
strands:
  weave: auto
  pulse: auto
EOF

    clear_config_cache

    local weave_enabled pulse_enabled
    _needle_billing_is_strand_enabled "weave" "use_or_lose" && weave_enabled=1 || weave_enabled=0
    _needle_billing_is_strand_enabled "pulse" "use_or_lose" && pulse_enabled=1 || pulse_enabled=0

    if [[ $weave_enabled -eq 1 && $pulse_enabled -eq 1 ]]; then
        _test_pass "$test_name - all strands enabled in use_or_lose"
    else
        _test_fail "$test_name - all strands should be enabled in use_or_lose"
    fi
}

# =============================================================================
# Test: _needle_billing_is_strand_enabled - unlimited
# =============================================================================
test_strand_enabled_unlimited_weave() {
    local test_name="strand_enabled_unlimited_weave"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: unlimited
strands:
  weave: auto
EOF

    clear_config_cache

    if _needle_billing_is_strand_enabled "weave" "unlimited"; then
        _test_fail "$test_name - weave should be disabled in unlimited (opt-in)"
    else
        _test_pass "$test_name - weave disabled in unlimited (opt-in)"
    fi
}

test_strand_enabled_unlimited_unravel() {
    local test_name="strand_enabled_unlimited_unravel"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: unlimited
strands:
  unravel: auto
EOF

    clear_config_cache

    if _needle_billing_is_strand_enabled "unravel" "unlimited"; then
        _test_pass "$test_name - unravel enabled in unlimited"
    else
        _test_fail "$test_name - unravel should be enabled in unlimited"
    fi
}

test_strand_enabled_unlimited_explore() {
    local test_name="strand_enabled_unlimited_explore"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: unlimited
strands:
  explore: auto
EOF

    clear_config_cache

    if _needle_billing_is_strand_enabled "explore" "unlimited"; then
        _test_pass "$test_name - explore enabled in unlimited"
    else
        _test_fail "$test_name - explore should be enabled in unlimited"
    fi
}

test_strand_enabled_unlimited_essential() {
    local test_name="strand_enabled_unlimited_essential"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: unlimited
strands:
  pluck: auto
  mend: auto
  knot: auto
EOF

    clear_config_cache

    local pluck_enabled mend_enabled knot_enabled
    _needle_billing_is_strand_enabled "pluck" "unlimited" && pluck_enabled=1 || pluck_enabled=0
    _needle_billing_is_strand_enabled "mend" "unlimited" && mend_enabled=1 || mend_enabled=0
    _needle_billing_is_strand_enabled "knot" "unlimited" && knot_enabled=1 || knot_enabled=0

    if [[ $pluck_enabled -eq 1 && $mend_enabled -eq 1 && $knot_enabled -eq 1 ]]; then
        _test_pass "$test_name - essential strands enabled in unlimited"
    else
        _test_fail "$test_name - essential strands should be enabled in unlimited"
    fi
}

# =============================================================================
# Test: _needle_billing_is_strand_enabled - explicit override
# =============================================================================
test_strand_enabled_explicit_true() {
    local test_name="strand_enabled_explicit_true"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: pay_per_token
strands:
  weave: true
EOF

    clear_config_cache

    if _needle_billing_is_strand_enabled "weave" "pay_per_token"; then
        _test_pass "$test_name - explicit true overrides billing model"
    else
        _test_fail "$test_name - explicit true should override billing model"
    fi
}

test_strand_enabled_explicit_false() {
    local test_name="strand_enabled_explicit_false"

    cat > "$NEEDLE_CONFIG_FILE" << 'EOF'
billing:
  model: use_or_lose
strands:
  pluck: false
EOF

    clear_config_cache

    if _needle_billing_is_strand_enabled "pluck" "use_or_lose"; then
        _test_fail "$test_name - explicit false should override billing model"
    else
        _test_pass "$test_name - explicit false overrides billing model"
    fi
}

# =============================================================================
# Test: _needle_billing_should_stop_for_budget - strict
# =============================================================================
test_should_stop_strict_under_budget() {
    local test_name="should_stop_strict_under_budget"

    if _needle_billing_should_stop_for_budget "40" "50" "pay_per_token"; then
        _test_fail "$test_name - should continue under budget"
    else
        _test_pass "$test_name - continues under budget"
    fi
}

test_should_stop_strict_at_budget() {
    local test_name="should_stop_strict_at_budget"

    if _needle_billing_should_stop_for_budget "50" "50" "pay_per_token"; then
        _test_pass "$test_name - stops at 100% of budget"
    else
        _test_fail "$test_name - should stop at 100% of budget"
    fi
}

test_should_stop_strict_over_budget() {
    local test_name="should_stop_strict_over_budget"

    if _needle_billing_should_stop_for_budget "55" "50" "pay_per_token"; then
        _test_pass "$test_name - stops over budget"
    else
        _test_fail "$test_name - should stop over budget"
    fi
}

# =============================================================================
# Test: _needle_billing_should_stop_for_budget - target
# =============================================================================
test_should_stop_target_at_budget() {
    local test_name="should_stop_target_at_budget"

    if _needle_billing_should_stop_for_budget "50" "50" "use_or_lose"; then
        _test_fail "$test_name - should continue at 100% (allows overrun)"
    else
        _test_pass "$test_name - continues at 100% (target model)"
    fi
}

test_should_stop_target_at_120_percent() {
    local test_name="should_stop_target_at_120_percent"

    if _needle_billing_should_stop_for_budget "60" "50" "use_or_lose"; then
        _test_pass "$test_name - stops at 120% of budget"
    else
        _test_fail "$test_name - should stop at 120% of budget"
    fi
}

# =============================================================================
# Test: _needle_billing_should_stop_for_budget - none
# =============================================================================
test_should_stop_none() {
    local test_name="should_stop_none"

    if _needle_billing_should_stop_for_budget "1000" "50" "unlimited"; then
        _test_fail "$test_name - should never stop with unlimited"
    else
        _test_pass "$test_name - never stops with unlimited"
    fi
}

# =============================================================================
# Test: _needle_billing_get_priority_weight
# =============================================================================
test_priority_weight_pay_per_token_p0() {
    local test_name="priority_weight_pay_per_token_p0"

    local weight
    weight=$(_needle_billing_get_priority_weight 0 "pay_per_token")

    if [[ "$weight" == "8" ]]; then
        _test_pass "$test_name - P0 gets full weight (8)"
    else
        _test_fail "$test_name - expected 8, got $weight"
    fi
}

test_priority_weight_pay_per_token_p2() {
    local test_name="priority_weight_pay_per_token_p2"

    local weight
    weight=$(_needle_billing_get_priority_weight 2 "pay_per_token")

    if [[ "$weight" == "1" ]]; then
        _test_pass "$test_name - P2 gets reduced weight (1)"
    else
        _test_fail "$test_name - expected 1, got $weight"
    fi
}

test_priority_weight_use_or_lose_p0() {
    local test_name="priority_weight_use_or_lose_p0"

    local weight
    weight=$(_needle_billing_get_priority_weight 0 "use_or_lose")

    if [[ "$weight" == "12" ]]; then
        _test_pass "$test_name - P0 gets boosted weight (12)"
    else
        _test_fail "$test_name - expected 12, got $weight"
    fi
}

test_priority_weight_unlimited_p0() {
    local test_name="priority_weight_unlimited_p0"

    local weight
    weight=$(_needle_billing_get_priority_weight 0 "unlimited")

    if [[ "$weight" == "8" ]]; then
        _test_pass "$test_name - P0 gets base weight (8)"
    else
        _test_fail "$test_name - expected 8, got $weight"
    fi
}

# =============================================================================
# Main Test Runner
# =============================================================================

main() {
    echo "========================================="
    echo "NEEDLE Billing Models Test Suite"
    echo "========================================="
    echo ""

    setup

    # Run all tests
    test_get_billing_model_default
    test_get_billing_model_pay_per_token
    test_get_billing_model_use_or_lose
    test_get_billing_model_unlimited
    test_get_billing_budget
    test_enforcement_strategy_pay_per_token
    test_enforcement_strategy_use_or_lose
    test_enforcement_strategy_unlimited
    test_min_priority_pay_per_token
    test_min_priority_use_or_lose
    test_min_priority_unlimited
    test_concurrency_pay_per_token
    test_concurrency_use_or_lose
    test_concurrency_unlimited
    test_strand_enabled_pay_per_token_pluck
    test_strand_enabled_pay_per_token_weave
    test_strand_enabled_use_or_lose_all
    test_strand_enabled_unlimited_weave
    test_strand_enabled_unlimited_unravel
    test_strand_enabled_unlimited_explore
    test_strand_enabled_unlimited_essential
    test_strand_enabled_explicit_true
    test_strand_enabled_explicit_false
    test_should_stop_strict_under_budget
    test_should_stop_strict_at_budget
    test_should_stop_strict_over_budget
    test_should_stop_target_at_budget
    test_should_stop_target_at_120_percent
    test_should_stop_none
    test_priority_weight_pay_per_token_p0
    test_priority_weight_pay_per_token_p2
    test_priority_weight_use_or_lose_p0
    test_priority_weight_unlimited_p0

    teardown

    echo ""
    echo "========================================="
    echo "Test Results:"
    echo "  PASSED: $passed"
    echo "  FAILED: $failed"
    echo "========================================="

    if [[ $failed -eq 0 ]]; then
        exit 0
    else
        exit 1
    fi
}

# Run tests if executed directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
