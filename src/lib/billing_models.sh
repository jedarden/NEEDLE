#!/usr/bin/env bash
# NEEDLE Billing Model Profiles
# Adjust strand execution and priority handling based on billing model
#
# This module implements three billing model behavior profiles:
# - pay_per_token (default): Conservative, minimize token usage
# - use_or_lose: Aggressive, use allocated budget
# - unlimited: Maximum throughput
#
# Each model adjusts:
# - Strand enablement (which strands are active)
# - Priority thresholds (which priorities get processed)
# - Budget enforcement strategy (strict vs target vs none)
# - Worker concurrency defaults

# Source dependencies if not already loaded
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/output.sh"
fi

if [[ -z "${_NEEDLE_CONFIG_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/config.sh"
fi

# Module version
_NEEDLE_BILLING_MODELS_VERSION="1.0.0"
_NEEDLE_BILLING_MODELS_LOADED="true"

# -----------------------------------------------------------------------------
# Billing Model Configuration
# -----------------------------------------------------------------------------

# Get configured billing model
# Usage: get_billing_model
# Returns: pay_per_token | use_or_lose | unlimited (default: pay_per_token)
get_billing_model() {
    local model
    model=$(get_config "billing.model" "pay_per_token")

    # Validate model
    case "$model" in
        pay_per_token|use_or_lose|unlimited)
            echo "$model"
            ;;
        *)
            _needle_warn "Invalid billing model '$model', using pay_per_token"
            echo "pay_per_token"
            ;;
    esac
}

# Get billing model daily budget
# Usage: get_billing_budget
# Returns: Daily budget in USD (from billing.daily_budget_usd or effort.budget.daily_limit_usd)
get_billing_budget() {
    # Try billing.daily_budget_usd first (new config), fallback to effort.budget.daily_limit_usd
    local budget
    budget=$(get_config "billing.daily_budget_usd" "")

    if [[ -z "$budget" || "$budget" == "null" ]]; then
        budget=$(get_config "effort.budget.daily_limit_usd" "50.0")
    fi

    echo "$budget"
}

# -----------------------------------------------------------------------------
# Billing Model Profiles
# -----------------------------------------------------------------------------

# Get priority weight multiplier for billing model
# Usage: _needle_billing_get_priority_weight <priority> <model>
# Returns: Weight multiplier adjusted for billing model
_needle_billing_get_priority_weight() {
    local priority="${1:-2}"
    local model="${2:-$(get_billing_model)}"

    # Base weights: P0=8x, P1=4x, P2=2x, P3=1x, P4+=1x
    local base_weights=(8 4 2 1 1)

    # Validate priority is a number
    if ! [[ "$priority" =~ ^[0-9]+$ ]]; then
        priority=2
    fi

    # Cap at max defined priority
    if [[ $priority -ge ${#base_weights[@]} ]]; then
        priority=$(( ${#base_weights[@]} - 1 ))
    fi

    local base_weight="${base_weights[$priority]}"

    # Apply model-specific adjustments
    case "$model" in
        pay_per_token)
            # Conservative: reduce weight for lower priorities (only P0-P1 get full weight)
            if [[ $priority -ge 2 ]]; then
                # P2+ get half weight (round down)
                echo $(( base_weight / 2 ))
            else
                echo "$base_weight"
            fi
            ;;
        use_or_lose)
            # Aggressive: boost weight for all priorities
            # P0-P2 get 1.5x boost, P3+ get normal weight
            if [[ $priority -le 2 ]]; then
                echo $(( base_weight + base_weight / 2 ))
            else
                echo "$base_weight"
            fi
            ;;
        unlimited)
            # Maximum throughput: all priorities get equal high weight
            echo "$base_weight"
            ;;
        *)
            echo "$base_weight"
            ;;
    esac
}

# Get minimum priority threshold for billing model
# Usage: _needle_billing_get_min_priority <model>
# Returns: Minimum priority level to process (0-4, lower is higher priority)
_needle_billing_get_min_priority() {
    local model="${1:-$(get_billing_model)}"

    case "$model" in
        pay_per_token)
            # Conservative: only process P0-P1 by default (critical and high)
            echo "1"
            ;;
        use_or_lose)
            # Aggressive: process P0-P2 (critical, high, normal)
            echo "2"
            ;;
        unlimited)
            # Maximum: process all priorities (P0-P4+)
            echo "4"
            ;;
        *)
            echo "2"  # Default to normal
            ;;
    esac
}

# Check if a strand should be enabled for billing model
# Usage: _needle_billing_is_strand_enabled <strand_name> [model]
# Returns: 0 if enabled, 1 if disabled
_needle_billing_is_strand_enabled() {
    local strand="$1"
    local model="${2:-$(get_billing_model)}"

    # Get explicit config value (auto means follow billing model)
    local config_value
    config_value=$(get_config "strands.$strand" "auto")

    # If explicitly set (not auto), respect that setting
    case "$config_value" in
        true|True|TRUE|yes|Yes|YES|1)
            return 0
            ;;
        false|False|FALSE|no|No|NO|0)
            return 1
            ;;
        auto|Auto|AUTO)
            # Follow billing model profile
            ;;
        *)
            # Unknown value, treat as auto
            ;;
    esac

    # Apply billing model defaults
    case "$model" in
        pay_per_token)
            # Conservative: only essential strands (pluck, explore, mend, knot)
            case "$strand" in
                pluck|explore|mend|knot)
                    return 0
                    ;;
                weave|unravel|pulse)
                    return 1  # Opt-in strands disabled
                    ;;
                *)
                    return 0  # Unknown strands default to enabled
                    ;;
            esac
            ;;
        use_or_lose)
            # Aggressive: enable all strands including opt-in
            return 0
            ;;
        unlimited)
            # Balanced: enable everything except weave (still opt-in)
            case "$strand" in
                weave)
                    return 1  # Weave is opt-in even for unlimited
                    ;;
                *)
                    return 0  # All other strands enabled
                    ;;
            esac
            ;;
        *)
            # Default: conservative profile
            case "$strand" in
                pluck|explore|mend|knot)
                    return 0
                    ;;
                *)
                    return 1
                    ;;
            esac
            ;;
    esac
}

# Get budget enforcement strategy for billing model
# Usage: _needle_billing_get_enforcement_strategy [model]
# Returns: strict | target | none
_needle_billing_get_enforcement_strategy() {
    local model="${1:-$(get_billing_model)}"

    case "$model" in
        pay_per_token)
            # Conservative: strict enforcement (hard stop at budget)
            echo "strict"
            ;;
        use_or_lose)
            # Aggressive: target enforcement (budget is goal, not limit)
            echo "target"
            ;;
        unlimited)
            # Maximum: no enforcement
            echo "none"
            ;;
        *)
            echo "strict"
            ;;
    esac
}

# Get worker concurrency default for billing model
# Usage: _needle_billing_get_concurrency [model]
# Returns: Default max concurrent workers
_needle_billing_get_concurrency() {
    local model="${1:-$(get_billing_model)}"

    # Get configured value first
    local configured
    configured=$(get_config "limits.global_max_concurrent" "")

    # If explicitly configured, use that
    if [[ -n "$configured" && "$configured" != "null" ]]; then
        echo "$configured"
        return 0
    fi

    # Otherwise use billing model defaults
    case "$model" in
        pay_per_token)
            # Conservative: lower concurrency to minimize costs
            echo "3"
            ;;
        use_or_lose)
            # Aggressive: higher concurrency to use budget
            echo "8"
            ;;
        unlimited)
            # Maximum: highest concurrency
            echo "20"
            ;;
        *)
            echo "5"
            ;;
    esac
}

# -----------------------------------------------------------------------------
# Budget Enforcement with Billing Model
# -----------------------------------------------------------------------------

# Check if budget should stop work (respects billing model)
# Usage: _needle_billing_should_stop_for_budget <daily_spend> <daily_limit> [model]
# Returns: 0 if should stop, 1 if should continue
_needle_billing_should_stop_for_budget() {
    local daily_spend="$1"
    local daily_limit="$2"
    local model="${3:-$(get_billing_model)}"
    local strategy

    strategy=$(_needle_billing_get_enforcement_strategy "$model")

    case "$strategy" in
        strict)
            # Strict: stop at 100% of budget
            if command -v bc &>/dev/null; then
                local exceeded
                exceeded=$(echo "$daily_spend >= $daily_limit" | bc -l 2>/dev/null)
                [[ "$exceeded" == "1" ]]
            elif command -v awk &>/dev/null; then
                # Use awk for float comparison
                awk "BEGIN {exit !($daily_spend >= $daily_limit)}"
            else
                # Fallback: integer comparison
                local spend_int limit_int
                spend_int=$(echo "$daily_spend" | sed 's/\..*//')
                limit_int=$(echo "$daily_limit" | sed 's/\..*//')
                [[ ${spend_int:-0} -ge ${limit_int:-0} ]]
            fi
            ;;
        target)
            # Target: only stop at 120% of budget (allow overrun)
            if command -v bc &>/dev/null; then
                local threshold
                threshold=$(echo "$daily_limit * 1.2" | bc -l 2>/dev/null)
                local exceeded
                exceeded=$(echo "$daily_spend >= $threshold" | bc -l 2>/dev/null)
                [[ "$exceeded" == "1" ]]
            elif command -v awk &>/dev/null; then
                # Use awk for float comparison
                awk "BEGIN {exit !($daily_spend >= $daily_limit * 1.2)}"
            else
                # Fallback: integer comparison at 120%
                local spend_int limit_int threshold_int
                spend_int=$(echo "$daily_spend" | sed 's/\..*//')
                limit_int=$(echo "$daily_limit" | sed 's/\..*//')
                threshold_int=$(( limit_int * 120 / 100 ))
                [[ ${spend_int:-0} -ge ${threshold_int:-0} ]]
            fi
            ;;
        none)
            # No enforcement: never stop
            return 1
            ;;
        *)
            # Unknown: default to strict
            if command -v bc &>/dev/null; then
                local exceeded
                exceeded=$(echo "$daily_spend >= $daily_limit" | bc -l 2>/dev/null)
                [[ "$exceeded" == "1" ]]
            else
                return 1
            fi
            ;;
    esac
}

# -----------------------------------------------------------------------------
# Billing Model Summary
# -----------------------------------------------------------------------------

# Get billing model profile summary as JSON
# Usage: get_billing_model_profile
# Returns: JSON object with current billing model profile
get_billing_model_profile() {
    local model daily_budget strategy min_priority concurrency

    model=$(get_billing_model)
    daily_budget=$(get_billing_budget)
    strategy=$(_needle_billing_get_enforcement_strategy "$model")
    min_priority=$(_needle_billing_get_min_priority "$model")
    concurrency=$(_needle_billing_get_concurrency "$model")

    # Get strand enablement
    local strands=(pluck explore mend weave unravel pulse knot)
    local enabled_strands=()
    local disabled_strands=()

    for strand in "${strands[@]}"; do
        if _needle_billing_is_strand_enabled "$strand" "$model"; then
            enabled_strands+=("$strand")
        else
            disabled_strands+=("$strand")
        fi
    done

    # Build JSON
    if command -v jq &>/dev/null; then
        jq -nc \
            --arg model "$model" \
            --arg daily_budget "$daily_budget" \
            --arg strategy "$strategy" \
            --arg min_priority "$min_priority" \
            --arg concurrency "$concurrency" \
            --argjson enabled_strands "$(printf '%s\n' "${enabled_strands[@]}" | jq -R . | jq -s .)" \
            --argjson disabled_strands "$(printf '%s\n' "${disabled_strands[@]}" | jq -R . | jq -s .)" \
            '{
                model: $model,
                daily_budget_usd: ($daily_budget | tonumber),
                enforcement_strategy: $strategy,
                min_priority: ($min_priority | tonumber),
                default_concurrency: ($concurrency | tonumber),
                enabled_strands: $enabled_strands,
                disabled_strands: $disabled_strands
            }'
    else
        # Fallback: manual JSON
        printf '{"model":"%s","daily_budget_usd":%s,"enforcement_strategy":"%s","min_priority":%s,"default_concurrency":%s}' \
            "$model" "$daily_budget" "$strategy" "$min_priority" "$concurrency"
    fi
}

# Display billing model profile
# Usage: show_billing_model_profile
show_billing_model_profile() {
    local model daily_budget strategy min_priority concurrency

    model=$(get_billing_model)
    daily_budget=$(get_billing_budget)
    strategy=$(_needle_billing_get_enforcement_strategy "$model")
    min_priority=$(_needle_billing_get_min_priority "$model")
    concurrency=$(_needle_billing_get_concurrency "$model")

    _needle_header "Billing Model Profile"

    _needle_table_row "Model" "$model"
    _needle_table_row "Daily Budget" "\$$$daily_budget"
    _needle_table_row "Enforcement" "$strategy"
    _needle_table_row "Min Priority" "P$min_priority and above"
    _needle_table_row "Concurrency" "$concurrency workers"

    _needle_section "Strand Configuration"

    local strands=(pluck explore mend weave unravel pulse knot)
    for strand in "${strands[@]}"; do
        if _needle_billing_is_strand_enabled "$strand" "$model"; then
            _needle_print_color "$NEEDLE_COLOR_GREEN" "  ✓ $strand (enabled)"
        else
            _needle_print_color "$NEEDLE_COLOR_DIM" "  ✗ $strand (disabled)"
        fi
    done
}

# -----------------------------------------------------------------------------
# Direct Execution Support (for testing)
# -----------------------------------------------------------------------------

# Allow running this module directly for testing
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    # Source dependencies for standalone testing
    source "$(dirname "${BASH_SOURCE[0]}")/output.sh"
    source "$(dirname "${BASH_SOURCE[0]}")/constants.sh"
    source "$(dirname "${BASH_SOURCE[0]}")/config.sh"

    case "${1:-}" in
        model)
            get_billing_model
            ;;
        budget)
            get_billing_budget
            ;;
        profile)
            get_billing_model_profile
            ;;
        show)
            show_billing_model_profile
            ;;
        strategy)
            _needle_billing_get_enforcement_strategy "${2:-}"
            ;;
        min-priority)
            _needle_billing_get_min_priority "${2:-}"
            ;;
        concurrency)
            _needle_billing_get_concurrency "${2:-}"
            ;;
        strand-enabled)
            if [[ $# -lt 2 ]]; then
                echo "Usage: $0 strand-enabled <strand_name> [model]"
                exit 1
            fi
            if _needle_billing_is_strand_enabled "$2" "${3:-}"; then
                echo "enabled"
            else
                echo "disabled"
            fi
            ;;
        should-stop)
            if [[ $# -lt 3 ]]; then
                echo "Usage: $0 should-stop <daily_spend> <daily_limit> [model]"
                exit 1
            fi
            if _needle_billing_should_stop_for_budget "$2" "$3" "${4:-}"; then
                echo "STOP"
            else
                echo "CONTINUE"
            fi
            ;;
        -h|--help)
            echo "Usage: $0 <command> [args]"
            echo ""
            echo "Commands:"
            echo "  model                          Get current billing model"
            echo "  budget                         Get daily budget"
            echo "  profile                        Get billing model profile as JSON"
            echo "  show                           Display billing model profile"
            echo "  strategy [model]               Get budget enforcement strategy"
            echo "  min-priority [model]           Get minimum priority threshold"
            echo "  concurrency [model]            Get default concurrency"
            echo "  strand-enabled <strand> [model] Check if strand is enabled"
            echo "  should-stop <spend> <limit> [model] Check if should stop for budget"
            ;;
        *)
            echo "Unknown command: ${1:-}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
fi
