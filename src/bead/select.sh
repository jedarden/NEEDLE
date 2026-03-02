#!/usr/bin/env bash
# NEEDLE Bead Selection Module
# Weighted bead selection from queue using priority-based random selection
#
# This module implements the pluck strand's selection logic:
# - Higher priority beads are selected more frequently
# - Uses weighted random selection to prevent starvation
# - P0=8x, P1=4x, P2=2x, P3=1x weight multipliers

# Source dependencies (if not already loaded)
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

if [[ -z "${_NEEDLE_CONSTANTS_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/constants.sh"
fi

# Priority weight configuration
# P0 (critical) = 8x, P1 (high) = 4x, P2 (normal) = 2x, P3 (low) = 1x
NEEDLE_PRIORITY_WEIGHTS=(
    8   # P0 - critical
    4   # P1 - high
    2   # P2 - normal
    1   # P3 - low
    1   # P4+ - backlog (same as P3)
)

# Get weight for a given priority level
# Usage: _needle_get_priority_weight <priority>
# Returns: weight multiplier (1-8)
_needle_get_priority_weight() {
    local priority="${1:-2}"  # Default to P2 (normal)

    # Validate priority is a number
    if ! [[ "$priority" =~ ^[0-9]+$ ]]; then
        priority=2
    fi

    # Cap at max defined priority (P4+ all get weight 1)
    if [[ $priority -ge ${#NEEDLE_PRIORITY_WEIGHTS[@]} ]]; then
        priority=$(( ${#NEEDLE_PRIORITY_WEIGHTS[@]} - 1 ))
    fi

    echo "${NEEDLE_PRIORITY_WEIGHTS[$priority]}"
}

# Select a bead from the ready queue using weighted random selection
# Usage: _needle_select_weighted [--json]
# Returns: bead ID (or full JSON object if --json specified)
# Exit codes:
#   0 - Success, bead selected
#   1 - No beads available or error
#
# Example:
#   bead_id=$(_needle_select_weighted)
#   bead_json=$(_needle_select_weighted --json)
_needle_select_weighted() {
    local output_json=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --json)
                output_json=true
                shift
                ;;
            *)
                shift
                ;;
        esac
    done

    # Get claimable beads from br CLI
    local candidates
    candidates=$(br ready --unassigned --json 2>/dev/null)

    # Handle empty or invalid response
    if [[ -z "$candidates" ]] || [[ "$candidates" == "[]" ]] || [[ "$candidates" == "null" ]]; then
        _needle_debug "No claimable beads available"
        return 1
    fi

    # Validate JSON structure
    if ! echo "$candidates" | jq -e '.[0]' &>/dev/null; then
        _needle_warn "Invalid response from br ready: expected JSON array"
        return 1
    fi

    # Count candidates
    local candidate_count
    candidate_count=$(echo "$candidates" | jq 'length')

    if [[ $candidate_count -eq 0 ]]; then
        _needle_debug "Empty bead queue"
        return 1
    fi

    _needle_debug "Found $candidate_count claimable bead(s)"

    # Build weighted array based on priority
    # Higher priority beads appear more times in the weighted array
    local weighted=()
    local selected_bead_id=""
    local selected_bead_json=""

    while IFS= read -r bead; do
        local id priority weight

        id=$(echo "$bead" | jq -r '.id // empty')
        [[ -z "$id" ]] && continue

        priority=$(echo "$bead" | jq -r '.priority // 2')
        weight=$(_needle_get_priority_weight "$priority")

        # Add bead ID to weighted array 'weight' times
        for ((i=0; i<weight; i++)); do
            weighted+=("$id")
        done

        _needle_verbose "Bead $id: priority=$priority, weight=$weight"

    done < <(echo "$candidates" | jq -c '.[]')

    # Check if we have any weighted entries
    if [[ ${#weighted[@]} -eq 0 ]]; then
        _needle_warn "No valid beads after weighting"
        return 1
    fi

    # Random selection from weighted array
    # RANDOM is a bash built-in that returns 0-32767
    local idx=$((RANDOM % ${#weighted[@]}))
    selected_bead_id="${weighted[$idx]}"

    _needle_debug "Selected bead: $selected_bead_id (from ${#weighted[@]} weighted entries)"

    # Get full bead JSON if --json was specified
    if [[ "$output_json" == "true" ]]; then
        selected_bead_json=$(echo "$candidates" | jq -c --arg id "$selected_bead_id" '.[] | select(.id == $id)')
        if [[ -n "$selected_bead_json" ]]; then
            echo "$selected_bead_json"
            return 0
        else
            # Fallback to just the ID if JSON extraction fails
            _needle_warn "Could not extract full JSON for bead $selected_bead_id"
            echo "$selected_bead_id"
            return 0
        fi
    fi

    echo "$selected_bead_id"
    return 0
}

# List all claimable beads with their weights
# Usage: _needle_list_weighted_beads
# Returns: JSON array of beads with computed weights
_needle_list_weighted_beads() {
    local candidates
    candidates=$(br ready --unassigned --json 2>/dev/null)

    if [[ -z "$candidates" ]] || [[ "$candidates" == "[]" ]] || [[ "$candidates" == "null" ]]; then
        echo "[]"
        return 0
    fi

    # Add weight to each bead and output
    echo "$candidates" | jq -c '.[]' | while IFS= read -r bead; do
        local priority weight
        priority=$(echo "$bead" | jq -r '.priority // 2')
        weight=$(_needle_get_priority_weight "$priority")
        echo "$bead" | jq -c --argjson w "$weight" '. + {weight: $w}'
    done | jq -s '.'
}

# Get statistics about the weighted bead pool
# Usage: _needle_select_stats
# Returns: JSON object with selection statistics
_needle_select_stats() {
    local candidates
    candidates=$(br ready --unassigned --json 2>/dev/null)

    if [[ -z "$candidates" ]] || [[ "$candidates" == "[]" ]] || [[ "$candidates" == "null" ]]; then
        echo '{"total_beads":0,"weighted_pool_size":0,"by_priority":{}}'
        return 0
    fi

    local total_beads weighted_pool_size
    total_beads=$(echo "$candidates" | jq 'length')
    weighted_pool_size=0

    # Count by priority and calculate weighted pool size
    declare -A priority_counts
    local priority bead_weight

    while IFS= read -r bead; do
        priority=$(echo "$bead" | jq -r '.priority // 2')
        bead_weight=$(_needle_get_priority_weight "$priority")
        weighted_pool_size=$((weighted_pool_size + bead_weight))
        priority_counts[$priority]=$((${priority_counts[$priority]:-0} + 1))
    done < <(echo "$candidates" | jq -c '.[]')

    # Build statistics JSON
    local by_priority_json="{"
    local first=true
    for p in "${!priority_counts[@]}"; do
        if [[ "$first" == "true" ]]; then
            first=false
        else
            by_priority_json+=","
        fi
        local w=$(_needle_get_priority_weight "$p")
        by_priority_json+="\"P$p\":{\"count\":${priority_counts[$p]},\"weight\":$w}"
    done
    by_priority_json+="}"

    echo "{\"total_beads\":$total_beads,\"weighted_pool_size\":$weighted_pool_size,\"by_priority\":$by_priority_json}"
}

# Direct execution support (for testing)
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-}" in
        --stats)
            _needle_select_stats | jq .
            ;;
        --list)
            _needle_list_weighted_beads | jq .
            ;;
        --json)
            _needle_select_weighted --json | jq .
            ;;
        -h|--help)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  (no args)   Select a bead using weighted random selection"
            echo "  --json      Output selected bead as full JSON object"
            echo "  --list      List all claimable beads with weights"
            echo "  --stats     Show selection pool statistics"
            echo "  -h, --help  Show this help message"
            ;;
        *)
            _needle_select_weighted "$@"
            ;;
    esac
fi
