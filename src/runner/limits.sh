#!/usr/bin/env bash
# NEEDLE Concurrency Limit Enforcement
# Enforces provider, model, and global concurrency limits
#
# This module provides functions to check and enforce concurrency limits
# before starting new workers. It integrates with:
# - Worker state registry (src/runner/state.sh)
# - Configuration system (src/lib/config.sh)
# - Agent loader (src/agent/loader.sh)

# -----------------------------------------------------------------------------
# Limit Retrieval Functions
# -----------------------------------------------------------------------------

# Get the global max concurrent limit
# Returns: Integer limit (default: 20)
# Usage: _needle_get_global_limit
_needle_get_global_limit() {
    local limit
    limit=$(get_config_int "limits.global_max_concurrent" "20")
    echo "${limit:-20}"
}

# Get the provider-specific max concurrent limit
# Arguments:
#   $1 - Provider name (e.g., "anthropic", "openai")
# Returns: Integer limit (default: 10)
# Usage: _needle_get_provider_limit "anthropic"
_needle_get_provider_limit() {
    local provider="$1"
    local limit

    if [[ -z "$provider" ]]; then
        echo "10"
        return 0
    fi

    limit=$(get_config_int "limits.providers.$provider.max_concurrent" "10")
    echo "${limit:-10}"
}

# Get the model-specific max concurrent limit
# Falls back to provider limit if model limit not set
# Arguments:
#   $1 - Agent name (e.g., "claude-anthropic-sonnet")
#   $2 - Provider name (optional, for fallback)
# Returns: Integer limit
# Usage: _needle_get_model_limit "claude-anthropic-sonnet" "anthropic"
_needle_get_model_limit() {
    local agent="$1"
    local provider="${2:-}"
    local limit

    if [[ -z "$agent" ]]; then
        _needle_get_provider_limit "$provider"
        return 0
    fi

    # Try agent-specific limit from config
    limit=$(get_config_int "limits.models.$agent.max_concurrent" "")

    if [[ -n "$limit" ]] && [[ "$limit" -gt 0 ]]; then
        echo "$limit"
        return 0
    fi

    # Fall back to provider limit
    _needle_get_provider_limit "$provider"
}

# Get model limit from agent config (loaded via agent loader)
# This checks the agent's own max_concurrent setting
# Arguments:
#   $1 - Agent name
# Returns: Integer limit (empty if agent not loaded)
# Usage: _needle_get_agent_config_limit "claude-anthropic-sonnet"
_needle_get_agent_config_limit() {
    local agent="$1"

    # If NEEDLE_AGENT is loaded and matches, use its limit
    if [[ "${NEEDLE_AGENT[name]:-}" == "$agent" ]] && [[ -n "${NEEDLE_AGENT[max_concurrent]:-}" ]]; then
        echo "${NEEDLE_AGENT[max_concurrent]}"
        return 0
    fi

    # Try to load agent and get limit
    if _needle_load_agent "$agent" &>/dev/null; then
        echo "${NEEDLE_AGENT[max_concurrent]:-5}"
        return 0
    fi

    echo ""
}

# Get effective model limit considering all sources
# Priority: global config model limit > agent config limit > provider limit
# Arguments:
#   $1 - Agent name
#   $2 - Provider name
# Returns: Integer limit
# Usage: _needle_get_effective_model_limit "claude-anthropic-sonnet" "anthropic"
_needle_get_effective_model_limit() {
    local agent="$1"
    local provider="$2"

    # First check global config for model-specific limit
    local config_limit
    config_limit=$(get_config_int "limits.models.$agent.max_concurrent" "")

    if [[ -n "$config_limit" ]] && [[ "$config_limit" -gt 0 ]]; then
        echo "$config_limit"
        return 0
    fi

    # Then check agent's own config
    local agent_limit
    agent_limit=$(_needle_get_agent_config_limit "$agent")

    if [[ -n "$agent_limit" ]] && [[ "$agent_limit" -gt 0 ]]; then
        echo "$agent_limit"
        return 0
    fi

    # Fall back to provider limit
    _needle_get_provider_limit "$provider"
}

# -----------------------------------------------------------------------------
# Current Count Functions
# -----------------------------------------------------------------------------

# Get current count of workers for a provider
# Arguments:
#   $1 - Provider name
# Returns: Integer count
# Usage: _needle_get_provider_count "anthropic"
_needle_get_provider_count() {
    local provider="$1"

    if [[ -z "$provider" ]]; then
        echo "0"
        return 0
    fi

    _needle_count_by_provider "$provider"
}

# Get current count of workers for an agent
# Arguments:
#   $1 - Agent name (e.g., "claude-anthropic-sonnet")
# Returns: Integer count
# Usage: _needle_get_agent_count "claude-anthropic-sonnet"
_needle_get_agent_count() {
    local agent="$1"

    if [[ -z "$agent" ]]; then
        echo "0"
        return 0
    fi

    _needle_count_by_agent "$agent"
}

# Get current global worker count
# Returns: Integer count
# Usage: _needle_get_global_count
_needle_get_global_count() {
    _needle_count_all_workers
}

# -----------------------------------------------------------------------------
# Limit Check Functions
# -----------------------------------------------------------------------------

# Check result structure (passed via nameref or globals)
NEEDLE_LIMIT_CHECK_PASSED=true
NEEDLE_LIMIT_CHECK_MESSAGE=""
NEEDLE_LIMIT_CHECK_DETAILS=""

# Check if global limit would be exceeded
# Arguments:
#   $1 - Number of additional workers to account for (default: 1)
# Returns: 0 if within limit, 1 if exceeded
# Sets: NEEDLE_LIMIT_CHECK_MESSAGE with details
# Usage: _needle_check_global_limit 1
_needle_check_global_limit() {
    local additional="${1:-1}"
    local global_limit global_count

    global_limit=$(_needle_get_global_limit)
    global_count=$(_needle_get_global_count)

    local projected=$((global_count + additional))

    if [[ $projected -gt $global_limit ]]; then
        NEEDLE_LIMIT_CHECK_PASSED=false
        NEEDLE_LIMIT_CHECK_MESSAGE="Global concurrency limit reached"
        NEEDLE_LIMIT_CHECK_DETAILS="Current: $global_count, Limit: $global_limit, Requested: +$additional"
        return 1
    fi

    NEEDLE_LIMIT_CHECK_DETAILS="Global: $global_count/$global_limit"
    return 0
}

# Check if provider limit would be exceeded
# Arguments:
#   $1 - Provider name
#   $2 - Number of additional workers (default: 1)
# Returns: 0 if within limit, 1 if exceeded
# Usage: _needle_check_provider_limit "anthropic" 1
_needle_check_provider_limit() {
    local provider="$1"
    local additional="${2:-1}"

    if [[ -z "$provider" ]]; then
        return 0
    fi

    local provider_limit provider_count

    provider_limit=$(_needle_get_provider_limit "$provider")
    provider_count=$(_needle_get_provider_count "$provider")

    local projected=$((provider_count + additional))

    if [[ $projected -gt $provider_limit ]]; then
        NEEDLE_LIMIT_CHECK_PASSED=false
        NEEDLE_LIMIT_CHECK_MESSAGE="Provider limit reached for '$provider'"
        NEEDLE_LIMIT_CHECK_DETAILS="Current: $provider_count, Limit: $provider_limit, Requested: +$additional"
        return 1
    fi

    NEEDLE_LIMIT_CHECK_DETAILS="${NEEDLE_LIMIT_CHECK_DETAILS:-}, Provider($provider): $provider_count/$provider_limit"
    return 0
}

# Check if model/agent limit would be exceeded
# Arguments:
#   $1 - Agent name
#   $2 - Provider name
#   $3 - Number of additional workers (default: 1)
# Returns: 0 if within limit, 1 if exceeded
# Usage: _needle_check_model_limit "claude-anthropic-sonnet" "anthropic" 1
_needle_check_model_limit() {
    local agent="$1"
    local provider="$2"
    local additional="${3:-1}"

    if [[ -z "$agent" ]]; then
        return 0
    fi

    local model_limit agent_count

    model_limit=$(_needle_get_effective_model_limit "$agent" "$provider")
    agent_count=$(_needle_get_agent_count "$agent")

    local projected=$((agent_count + additional))

    if [[ $projected -gt $model_limit ]]; then
        NEEDLE_LIMIT_CHECK_PASSED=false
        NEEDLE_LIMIT_CHECK_MESSAGE="Model limit reached for '$agent'"
        NEEDLE_LIMIT_CHECK_DETAILS="Current: $agent_count, Limit: $model_limit, Requested: +$additional"
        return 1
    fi

    NEEDLE_LIMIT_CHECK_DETAILS="${NEEDLE_LIMIT_CHECK_DETAILS:-}, Model($agent): $agent_count/$model_limit"
    return 0
}

# -----------------------------------------------------------------------------
# Main Concurrency Check Function
# -----------------------------------------------------------------------------

# Check all concurrency limits before starting a worker
# This is the main entry point for limit enforcement
#
# Arguments:
#   $1 - Agent name (e.g., "claude-anthropic-sonnet")
#   $2 - Provider name (optional, extracted from agent if not provided)
#   $3 - Number of additional workers (default: 1)
#
# Returns: 0 if all checks pass, 1 if any limit exceeded
#
# Sets globals:
#   NEEDLE_LIMIT_CHECK_PASSED - true/false
#   NEEDLE_LIMIT_CHECK_MESSAGE - Error message if failed
#   NEEDLE_LIMIT_CHECK_DETAILS - Detailed counts
#
# Usage:
#   if _needle_check_concurrency "claude-anthropic-sonnet"; then
#       echo "OK to start worker"
#   else
#       echo "Limit reached: $NEEDLE_LIMIT_CHECK_MESSAGE"
#   fi
_needle_check_concurrency() {
    local agent="$1"
    local provider="${2:-}"
    local additional="${3:-1}"

    # Reset check state
    NEEDLE_LIMIT_CHECK_PASSED=true
    NEEDLE_LIMIT_CHECK_MESSAGE=""
    NEEDLE_LIMIT_CHECK_DETAILS=""

    # Extract provider from agent name if not provided
    # Agent format: runner-provider-model (e.g., "claude-anthropic-sonnet")
    if [[ -z "$provider" ]] && [[ -n "$agent" ]]; then
        # Try to parse provider from agent name
        if [[ "$agent" =~ ^[a-z]+-([a-z]+)-[a-z0-9]+$ ]]; then
            provider="${BASH_REMATCH[1]}"
        fi
    fi

    # Ensure worker registry is initialized
    _needle_workers_init &>/dev/null || true

    # Check limits in order of specificity (model > provider > global)
    # This ensures the most specific error message is shown

    local model_failed=false
    local provider_failed=false
    local global_failed=false

    # Check model limit first
    if ! _needle_check_model_limit "$agent" "$provider" "$additional"; then
        model_failed=true
    fi

    # Check provider limit
    if ! _needle_check_provider_limit "$provider" "$additional"; then
        provider_failed=true
    fi

    # Check global limit
    if ! _needle_check_global_limit "$additional"; then
        global_failed=true
    fi

    # Return failure if any check failed
    if [[ "$model_failed" == "true" ]] || [[ "$provider_failed" == "true" ]] || [[ "$global_failed" == "true" ]]; then
        NEEDLE_LIMIT_CHECK_PASSED=false
        return 1
    fi

    return 0
}

# Convenience wrapper that outputs error message
# Use this for simple cases where you just want pass/fail with message
# Arguments:
#   $1 - Agent name
#   $2 - Provider name (optional)
#   $3 - Additional workers (optional, default: 1)
# Returns: 0 if OK, 1 if limit reached (prints error)
# Usage: _needle_enforce_concurrency "claude-anthropic-sonnet" "anthropic"
_needle_enforce_concurrency() {
    local agent="$1"
    local provider="${2:-}"
    local additional="${3:-1}"

    if _needle_check_concurrency "$agent" "$provider" "$additional"; then
        return 0
    fi

    # Output error message
    _needle_error "$NEEDLE_LIMIT_CHECK_MESSAGE"
    _needle_info "$NEEDLE_LIMIT_CHECK_DETAILS"

    return 1
}

# -----------------------------------------------------------------------------
# Status and Display Functions
# -----------------------------------------------------------------------------

# Get concurrency status as JSON
# Useful for status commands and debugging
# Arguments:
#   $1 - Agent name (optional, for specific status)
#   $2 - Provider name (optional)
# Returns: JSON object with limit info
# Usage: _needle_get_concurrency_status "claude-anthropic-sonnet"
_needle_get_concurrency_status() {
    local agent="${1:-}"
    local provider="${2:-}"

    # Extract provider from agent if needed
    if [[ -z "$provider" ]] && [[ -n "$agent" ]]; then
        if [[ "$agent" =~ ^[a-z]+-([a-z]+)-[a-z0-9]+$ ]]; then
            provider="${BASH_REMATCH[1]}"
        fi
    fi

    local global_limit global_count
    global_limit=$(_needle_get_global_limit)
    global_count=$(_needle_get_global_count)

    local provider_limit provider_count
    provider_limit=$(_needle_get_provider_limit "$provider")
    provider_count=$(_needle_get_provider_count "$provider")

    local model_limit agent_count
    if [[ -n "$agent" ]]; then
        model_limit=$(_needle_get_effective_model_limit "$agent" "$provider")
        agent_count=$(_needle_get_agent_count "$agent")
    else
        model_limit="null"
        agent_count="null"
    fi

    # Build JSON
    cat <<EOF
{
  "global": {
    "current": $global_count,
    "limit": $global_limit,
    "available": $((global_limit - global_count))
  },
  "provider": {
    "name": "$provider",
    "current": $provider_count,
    "limit": $provider_limit,
    "available": $((provider_limit - provider_count))
  },
  "model": {
    "name": "$agent",
    "current": ${agent_count:-null},
    "limit": ${model_limit:-null},
    "available": $([[ -n "$agent" ]] && echo $((model_limit - agent_count)) || echo "null")
  }
}
EOF
}

# Display concurrency status in human-readable format
# Arguments:
#   $1 - Agent name (optional)
#   $2 - Provider name (optional)
# Usage: _needle_show_concurrency_status "claude-anthropic-sonnet"
_needle_show_concurrency_status() {
    local agent="${1:-}"
    local provider="${2:-}"

    # Extract provider from agent if needed
    if [[ -z "$provider" ]] && [[ -n "$agent" ]]; then
        if [[ "$agent" =~ ^[a-z]+-([a-z]+)-[a-z0-9]+$ ]]; then
            provider="${BASH_REMATCH[1]}"
        fi
    fi

    local global_limit global_count
    global_limit=$(_needle_get_global_limit)
    global_count=$(_needle_get_global_count)

    local provider_limit provider_count
    provider_limit=$(_needle_get_provider_limit "$provider")
    provider_count=$(_needle_get_provider_count "$provider")

    _needle_section "Concurrency Status"

    # Global
    local global_pct=0
    if [[ $global_limit -gt 0 ]]; then
        global_pct=$((global_count * 100 / global_limit))
    fi
    local global_status="OK"
    [[ $global_pct -ge 80 ]] && global_status="WARNING"
    [[ $global_pct -ge 100 ]] && global_status="LIMIT"

    _needle_table_row "Global" "$global_count / $global_limit ($global_pct%) [$global_status]"

    # Provider
    if [[ -n "$provider" ]]; then
        local provider_pct=0
        if [[ $provider_limit -gt 0 ]]; then
            provider_pct=$((provider_count * 100 / provider_limit))
        fi
        local provider_status="OK"
        [[ $provider_pct -ge 80 ]] && provider_status="WARNING"
        [[ $provider_pct -ge 100 ]] && provider_status="LIMIT"

        _needle_table_row "Provider ($provider)" "$provider_count / $provider_limit ($provider_pct%) [$provider_status]"
    fi

    # Model
    if [[ -n "$agent" ]]; then
        local model_limit agent_count
        model_limit=$(_needle_get_effective_model_limit "$agent" "$provider")
        agent_count=$(_needle_get_agent_count "$agent")

        local model_pct=0
        if [[ $model_limit -gt 0 ]]; then
            model_pct=$((agent_count * 100 / model_limit))
        fi
        local model_status="OK"
        [[ $model_pct -ge 80 ]] && model_status="WARNING"
        [[ $model_pct -ge 100 ]] && model_status="LIMIT"

        _needle_table_row "Model ($agent)" "$agent_count / $model_limit ($model_pct%) [$model_status]"
    fi
}
