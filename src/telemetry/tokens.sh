#!/usr/bin/env bash
# NEEDLE Token Extraction Module
# Extracts token counts from agent output for cost tracking
#
# This module handles token extraction from different agent output formats:
# - JSON output (Claude Code style)
# - Text output with regex patterns
# - Handles missing token info gracefully

# Source dependencies if not already loaded
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

# Module version
_NEDDLE_TOKENS_VERSION="1.0.0"

# -----------------------------------------------------------------------------
# JSON Token Extraction
# -----------------------------------------------------------------------------

# Extract tokens from JSON output
# Handles various JSON structures used by different AI providers
#
# Usage: _needle_extract_tokens_json <output_file>
# Returns: input_tokens|output_tokens
#
# Supported JSON formats:
# - {"input_tokens": 1234, "output_tokens": 567}
# - {"usage": {"input_tokens": 1234, "output_tokens": 567}}
# - {"usage": {"prompt_tokens": 1234, "completion_tokens": 567}}
# - {"tokens": {"input": 1234, "output": 567}}
_needle_extract_tokens_json() {
    local output_file="$1"

    if [[ ! -f "$output_file" ]]; then
        echo "0|0"
        return 1
    fi

    local input_tokens=0
    local output_tokens=0

    # Try different JSON structures using jq if available
    if command -v jq &>/dev/null; then
        # Read file content once for efficiency
        local content
        content=$(cat "$output_file" 2>/dev/null)

        # Skip if content is empty or not valid JSON
        if [[ -z "$content" ]] || ! echo "$content" | jq empty 2>/dev/null; then
            echo "0|0"
            return 0
        fi

        # Try to extract input tokens - check multiple possible paths
        input_tokens=$(
            echo "$content" | jq -r '
                .input_tokens //
                .usage.input_tokens //
                .usage.prompt_tokens //
                .tokens.input //
                .prompt_tokens //
                0
            ' 2>/dev/null
        )

        # Try to extract output tokens - check multiple possible paths
        output_tokens=$(
            echo "$content" | jq -r '
                .output_tokens //
                .usage.output_tokens //
                .usage.completion_tokens //
                .tokens.output //
                .completion_tokens //
                0
            ' 2>/dev/null
        )

        # Ensure we have valid numbers
        [[ ! "$input_tokens" =~ ^[0-9]+$ ]] && input_tokens=0
        [[ ! "$output_tokens" =~ ^[0-9]+$ ]] && output_tokens=0
    else
        # Fallback: Use grep/sed for simple patterns
        input_tokens=$(_needle_extract_tokens_json_fallback "$output_file" "input")
        output_tokens=$(_needle_extract_tokens_json_fallback "$output_file" "output")
    fi

    echo "${input_tokens}|${output_tokens}"
    return 0
}

# Fallback JSON token extraction using grep/sed
# Usage: _needle_extract_tokens_json_fallback <output_file> <type>
# type: "input" or "output"
_needle_extract_tokens_json_fallback() {
    local output_file="$1"
    local type="$2"

    local value=0

    # Try various patterns
    case "$type" in
        input)
            # Try: input_tokens, usage.input_tokens, prompt_tokens
            value=$(grep -oE '"input_tokens"[[:space:]]*:[[:space:]]*[0-9]+' "$output_file" 2>/dev/null | grep -oE '[0-9]+' | head -1)
            [[ -z "$value" ]] && value=$(grep -oE '"prompt_tokens"[[:space:]]*:[[:space:]]*[0-9]+' "$output_file" 2>/dev/null | grep -oE '[0-9]+' | head -1)
            ;;
        output)
            # Try: output_tokens, usage.output_tokens, completion_tokens
            value=$(grep -oE '"output_tokens"[[:space:]]*:[[:space:]]*[0-9]+' "$output_file" 2>/dev/null | grep -oE '[0-9]+' | head -1)
            [[ -z "$value" ]] && value=$(grep -oE '"completion_tokens"[[:space:]]*:[[:space:]]*[0-9]+' "$output_file" 2>/dev/null | grep -oE '[0-9]+' | head -1)
            ;;
    esac

    echo "${value:-0}"
}

# -----------------------------------------------------------------------------
# Text/Regex Token Extraction
# -----------------------------------------------------------------------------

# Extract tokens from text output using regex pattern
#
# Usage: _needle_extract_tokens_text <output_file> <pattern>
# Returns: input_tokens|output_tokens
#
# Pattern should capture token counts with named groups or positional groups:
# - "tokens_used: ([0-9]+)" - single total
# - "input: ([0-9]+), output: ([0-9]+)" - input then output
# - "in=([0-9]+).*out=([0-9]+)" - input then output
_needle_extract_tokens_text() {
    local output_file="$1"
    local pattern="$2"

    if [[ ! -f "$output_file" ]]; then
        echo "0|0"
        return 1
    fi

    # If no pattern provided, try common patterns
    if [[ -z "$pattern" ]]; then
        _needle_extract_tokens_text_autodetect "$output_file"
        return $?
    fi

    local input_tokens=0
    local output_tokens=0

    # Convert file content to single line for multiline pattern matching
    # Replace newlines with spaces to allow patterns to match across lines
    local content
    content=$(tr '\n' ' ' < "$output_file" 2>/dev/null)

    # Try to match the pattern
    local match
    match=$(echo "$content" | grep -oE "$pattern" 2>/dev/null | head -1)

    if [[ -n "$match" ]]; then
        # Extract all numbers from the match
        local numbers
        numbers=$(echo "$match" | grep -oE '[0-9]+' 2>/dev/null | tr '\n' ' ')

        if [[ -n "$numbers" ]]; then
            # Read numbers into array
            local -a nums
            read -ra nums <<< "$numbers"

            case "${#nums[@]}" in
                1)
                    # Single number - treat as total or output tokens
                    output_tokens="${nums[0]}"
                    ;;
                2)
                    # Two numbers - assume input then output
                    input_tokens="${nums[0]}"
                    output_tokens="${nums[1]}"
                    ;;
                *)
                    # Multiple numbers - take first two as input/output
                    input_tokens="${nums[0]:-0}"
                    output_tokens="${nums[1]:-0}"
                    ;;
            esac
        fi
    fi

    echo "${input_tokens}|${output_tokens}"
    return 0
}

# Auto-detect tokens from text using common patterns
# Usage: _needle_extract_tokens_text_autodetect <output_file>
# Returns: input_tokens|output_tokens
_needle_extract_tokens_text_autodetect() {
    local output_file="$1"

    local input_tokens=0
    local output_tokens=0

    # Convert file content to single line for multiline pattern matching
    local content
    content=$(tr '\n' ' ' < "$output_file" 2>/dev/null)

    # Try common patterns in order of specificity
    local patterns=(
        # Format: "input: N, output: N" (with comma or other separator)
        '[Ii]nput[[:space:]]*:[[:space:]]*([0-9]+)[^0-9]*[Oo]utput[[:space:]]*:[[:space:]]*([0-9]+)'
        # Format: "tokens: N input, N output"
        '([0-9]+)[[:space:]]+input[[:space:]]+tokens?[^0-9]*([0-9]+)[[:space:]]+output[[:space:]]+tokens?'
        # Format: "in=N out=N"
        'in[[:space:]]*=[[:space:]]*([0-9]+)[^0-9]*out[[:space:]]*=[[:space:]]*([0-9]+)'
        # Format: "Input tokens: N\nOutput tokens: N"
        '[Ii]nput[[:space:]]+tokens?[[:space:]]*:[[:space:]]*([0-9]+)[^0-9]*[Oo]utput[[:space:]]+tokens?[[:space:]]*:[[:space:]]*([0-9]+)'
        # Format: "total tokens: N"
        '[Tt]otal[[:space:]]+[Tt]okens?[[:space:]]*:[[:space:]]*([0-9]+)'
        # Format: "tokens_used: N" (with underscore or space)
        '[Tt]okens?[_[:space:]]*[Uu]sed[[:space:]]*:[[:space:]]*([0-9]+)'
    )

    for pattern in "${patterns[@]}"; do
        local match
        match=$(echo "$content" | grep -oE "$pattern" 2>/dev/null | head -1)

        if [[ -n "$match" ]]; then
            local numbers
            numbers=$(echo "$match" | grep -oE '[0-9]+' 2>/dev/null | tr '\n' ' ')

            if [[ -n "$numbers" ]]; then
                local -a nums
                read -ra nums <<< "$numbers"

                case "${#nums[@]}" in
                    1)
                        output_tokens="${nums[0]}"
                        ;;
                    2)
                        input_tokens="${nums[0]}"
                        output_tokens="${nums[1]}"
                        ;;
                esac

                # If we found tokens, return
                if [[ $input_tokens -gt 0 ]] || [[ $output_tokens -gt 0 ]]; then
                    echo "${input_tokens}|${output_tokens}"
                    return 0
                fi
            fi
        fi
    done

    echo "0|0"
    return 0
}

# -----------------------------------------------------------------------------
# Main Entry Point
# -----------------------------------------------------------------------------

# Extract token counts from agent output
# This is the main function that should be called by other modules
#
# Usage: _needle_extract_tokens <agent_name> <output_file>
# Returns: input_tokens|output_tokens (pipe-delimited)
#
# The agent configuration determines the extraction method:
# - output.format: "json" or "text"
# - output.token_pattern: regex pattern for text format
#
# Example:
#   tokens=$(_needle_extract_tokens "claude-anthropic-sonnet" "/tmp/output.log")
#   input=$(echo "$tokens" | cut -d'|' -f1)
#   output=$(echo "$tokens" | cut -d'|' -f2)
_needle_extract_tokens() {
    local agent_name="$1"
    local output_file="$2"

    # Validate inputs
    if [[ -z "$agent_name" ]]; then
        _needle_warn "Cannot extract tokens: agent name required"
        echo "0|0"
        return 1
    fi

    if [[ -z "$output_file" ]]; then
        _needle_warn "Cannot extract tokens: output file required"
        echo "0|0"
        return 1
    fi

    if [[ ! -f "$output_file" ]]; then
        _needle_debug "Output file not found: $output_file"
        echo "0|0"
        return 1
    fi

    # Get agent configuration
    # First check if NEEDLE_AGENT is already populated for this agent
    local format=""
    local token_pattern=""

    # Source loader if not available
    if ! declare -p NEEDLE_AGENT &>/dev/null; then
        if [[ -f "$(dirname "${BASH_SOURCE[0]}")/../agent/loader.sh" ]]; then
            source "$(dirname "${BASH_SOURCE[0]}")/../agent/loader.sh"
        fi
    fi

    # Load agent config if not already loaded or different agent
    if [[ "${NEEDLE_AGENT[name]:-}" != "$agent_name" ]]; then
        if ! _needle_load_agent "$agent_name" 2>/dev/null; then
            _needle_debug "Could not load agent config for: $agent_name, using defaults"
        fi
    fi

    # Get format and pattern from agent config
    format="${NEEDLE_AGENT[output_format]:-text}"
    token_pattern="${NEEDLE_AGENT[token_pattern]:-}"

    _needle_debug "Extracting tokens: format=$format, pattern=$token_pattern"

    # Extract based on format
    case "$format" in
        json)
            _needle_extract_tokens_json "$output_file"
            ;;
        text|*)
            _needle_extract_tokens_text "$output_file" "$token_pattern"
            ;;
    esac
}

# Extract tokens from output file using explicit format
# Usage: _needle_extract_tokens_with_format <output_file> <format> [pattern]
# Returns: input_tokens|output_tokens
_needle_extract_tokens_with_format() {
    local output_file="$1"
    local format="$2"
    local pattern="${3:-}"

    if [[ ! -f "$output_file" ]]; then
        echo "0|0"
        return 1
    fi

    case "$format" in
        json)
            _needle_extract_tokens_json "$output_file"
            ;;
        text|*)
            _needle_extract_tokens_text "$output_file" "$pattern"
            ;;
    esac
}

# -----------------------------------------------------------------------------
# Utility Functions
# -----------------------------------------------------------------------------

# Parse token result string
# Usage: _needle_parse_token_result <result> <var_prefix>
# Sets: <var_prefix>_input, <var_prefix>_output, <var_prefix>_total
_needle_parse_token_result() {
    local result="$1"
    local prefix="$2"

    if [[ -z "$result" ]]; then
        eval "${prefix}_input=0"
        eval "${prefix}_output=0"
        eval "${prefix}_total=0"
        return 1
    fi

    local input output
    IFS='|' read -r input output <<< "$result"

    eval "${prefix}_input=${input:-0}"
    eval "${prefix}_output=${output:-0}"
    eval "${prefix}_total=$((input + output))"
}

# Calculate estimated cost from tokens
# Usage: _needle_calculate_token_cost <input_tokens> <output_tokens> <input_rate> <output_rate>
# input_rate: cost per 1M input tokens
# output_rate: cost per 1M output tokens
# Returns: cost in dollars (float)
_needle_calculate_token_cost() {
    local input_tokens="${1:-0}"
    local output_tokens="${2:-0}"
    local input_rate="${3:-3}"      # Default: $3/1M input tokens
    local output_rate="${4:-15}"    # Default: $15/1M output tokens

    # Calculate cost (tokens * rate / 1M)
    # Use bc for floating point if available, otherwise use awk
    if command -v bc &>/dev/null; then
        echo "scale=6; ($input_tokens * $input_rate / 1000000) + ($output_tokens * $output_rate / 1000000)" | bc 2>/dev/null
    else
        awk "BEGIN {printf \"%.6f\", ($input_tokens * $input_rate / 1000000) + ($output_tokens * $output_rate / 1000000)}" 2>/dev/null
    fi
}

# Get token statistics from a log file
# Usage: _needle_get_token_stats <output_file>
# Returns: JSON with input, output, total tokens
_needle_get_token_stats() {
    local output_file="$1"

    if [[ ! -f "$output_file" ]]; then
        echo '{"input":0,"output":0,"total":0}'
        return 1
    fi

    # Try JSON extraction first, then text
    local result
    result=$(_needle_extract_tokens_json "$output_file")

    local input output
    input=$(echo "$result" | cut -d'|' -f1)
    output=$(echo "$result" | cut -d'|' -f2)

    # If both are 0, try text extraction
    if [[ "$input" == "0" ]] && [[ "$output" == "0" ]]; then
        result=$(_needle_extract_tokens_text_autodetect "$output_file")
        input=$(echo "$result" | cut -d'|' -f1)
        output=$(echo "$result" | cut -d'|' -f2)
    fi

    echo "{\"input\":${input:-0},\"output\":${output:-0},\"total\":$((input + output))}"
}

# -----------------------------------------------------------------------------
# Direct Execution Support (for testing)
# -----------------------------------------------------------------------------

# Allow running this module directly for testing
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-}" in
        extract)
            if [[ $# -lt 2 ]]; then
                echo "Usage: $0 extract <output_file> [format] [pattern]"
                exit 1
            fi
            _needle_extract_tokens_with_format "$2" "${3:-json}" "${4:-}"
            ;;
        json)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 json <output_file>"
                exit 1
            fi
            _needle_extract_tokens_json "$2"
            ;;
        text)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 text <output_file> [pattern]"
                exit 1
            fi
            _needle_extract_tokens_text "$2" "${3:-}"
            ;;
        stats)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 stats <output_file>"
                exit 1
            fi
            _needle_get_token_stats "$2"
            ;;
        cost)
            if [[ $# -lt 3 ]]; then
                echo "Usage: $0 cost <input_tokens> <output_tokens> [input_rate] [output_rate]"
                exit 1
            fi
            _needle_calculate_token_cost "$2" "$3" "${4:-3}" "${5:-15}"
            ;;
        -h|--help)
            echo "Usage: $0 <command> [args]"
            echo ""
            echo "Commands:"
            echo "  extract <file> [format] [pattern]  Extract tokens from file"
            echo "  json <file>                        Extract from JSON format"
            echo "  text <file> [pattern]              Extract from text format"
            echo "  stats <file>                       Get token statistics as JSON"
            echo "  cost <in> <out> [in_rate] [out_rate]  Calculate cost"
            ;;
        *)
            echo "Unknown command: ${1:-}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
fi
