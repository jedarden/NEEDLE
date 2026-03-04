#!/usr/bin/env bash
# NEEDLE Bead Selection Module
# Weighted bead selection from queue using priority-based random selection
#
# This module implements the pluck strand's selection logic:
# - Higher priority beads are selected more frequently
# - Uses weighted random selection to prevent starvation
# - P0=8x, P1=4x, P2=2x, P3=1x weight multipliers

# ============================================================================
# PATH Setup (CRITICAL: Must be done before any br calls)
# ============================================================================
# Ensure ~/.local/bin is in PATH for br CLI access
# This fixes worker starvation caused by br not being found
if [[ -d "$HOME/.local/bin" ]]; then
    case ":$PATH:" in
        *":$HOME/.local/bin:"*) ;;
        *) export PATH="$HOME/.local/bin:$PATH" ;;
    esac
fi

# Verify br is available
if ! command -v br &>/dev/null; then
    echo "ERROR: br CLI not found in PATH" >&2
    echo "  PATH=$PATH" >&2
    echo "  Expected: $HOME/.local/bin/br" >&2
    echo "" >&2
    echo "Install br from: https://github.com/Dicklesworthstone/beads_rust" >&2
    exit 1
fi

# Source dependencies (if not already loaded)
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

if [[ -z "${_NEEDLE_CONSTANTS_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/constants.sh"
fi

# Source diagnostic module for logging
if [[ -z "${_NEEDLE_DIAGNOSTIC_LOADED:-}" ]]; then
    NEEDLE_SRC="${NEEDLE_SRC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
    source "$NEEDLE_SRC/lib/diagnostic.sh"
fi

# Source billing models module for priority weights
if [[ -z "${_NEEDLE_BILLING_MODELS_LOADED:-}" ]]; then
    billing_path="${NEEDLE_SRC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}/lib/billing_models.sh"
    if [[ -f "$billing_path" ]]; then
        source "$billing_path"
    fi
fi

# Source config module for work stealing settings
if [[ -z "${_NEEDLE_CONFIG_LOADED:-}" ]]; then
    config_path="${NEEDLE_SRC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}/lib/config.sh"
    if [[ -f "$config_path" ]]; then
        source "$config_path"
    fi
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

# Work stealing defaults (overridden by config)
NEEDLE_WORK_STEALING_ENABLED="${NEEDLE_WORK_STEALING_ENABLED:-true}"
NEEDLE_WORK_STEALING_TIMEOUT="${NEEDLE_WORK_STEALING_TIMEOUT:-1800}"  # 30 minutes
NEEDLE_STEALABLE_ASSIGNEES="${NEEDLE_STEALABLE_ASSIGNEES:-coder}"
NEEDLE_CHECK_WORKER_HEARTBEAT="${NEEDLE_CHECK_WORKER_HEARTBEAT:-true}"

# Get weight for a given priority level
# Usage: _needle_get_priority_weight <priority>
# Returns: weight multiplier (1-8)
_needle_get_priority_weight() {
    local priority="${1:-2}"  # Default to P2 (normal)

    # Use billing model module if available (adjusts weights based on billing model)
    if declare -f _needle_billing_get_priority_weight &>/dev/null; then
        _needle_billing_get_priority_weight "$priority"
        return 0
    fi

    # Fallback: use base priority weights
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

# ============================================================================
# Work Stealing Functions
# ============================================================================

# Check if work stealing is enabled
# Usage: _needle_work_stealing_enabled
# Returns: 0 if enabled, 1 if disabled
_needle_work_stealing_enabled() {
    local enabled
    enabled=$(get_config "select.work_stealing_enabled" "$NEEDLE_WORK_STEALING_ENABLED")

    case "$enabled" in
        true|True|TRUE|yes|Yes|YES|1) return 0 ;;
        *) return 1 ;;
    esac
}

# Get work stealing timeout in seconds
# Usage: _needle_get_work_stealing_timeout
# Returns: timeout in seconds (default: 1800 = 30 minutes)
_needle_get_work_stealing_timeout() {
    local timeout
    timeout=$(get_config "select.work_stealing_timeout" "$NEEDLE_WORK_STEALING_TIMEOUT")

    # Ensure it's a valid number
    if ! [[ "$timeout" =~ ^[0-9]+$ ]]; then
        timeout=1800
    fi

    echo "$timeout"
}

# Check if an assignee is in the stealable list
# Usage: _needle_is_stealable_assignee <assignee>
# Returns: 0 if stealable, 1 if not
_needle_is_stealable_assignee() {
    local assignee="$1"

    if [[ -z "$assignee" ]]; then
        return 1  # No assignee means unassigned, not stealable
    fi

    # Get stealable assignees from config
    local stealable_config
    stealable_config=$(get_config "select.stealable_assignees" "$NEEDLE_STEALABLE_ASSIGNEES")

    # Check if assignee is in the list (handles both JSON array and comma-separated string)
    if echo "$stealable_config" | jq -e --arg a "$assignee" 'index($a) != null' &>/dev/null; then
        return 0
    fi

    # Fallback: check as comma-separated string
    local IFS=','
    for stealable in $stealable_config; do
        # Remove quotes and whitespace
        stealable="${stealable//\"/}"
        stealable="${stealable// /}"
        if [[ "$assignee" == "$stealable" ]]; then
            return 0
        fi
    done

    return 1
}

# Check if a worker has a recent heartbeat
# Usage: _needle_worker_has_heartbeat <worker_name>
# Returns: 0 if heartbeat exists and is recent, 1 if no heartbeat or stale
_needle_worker_has_heartbeat() {
    local worker_name="$1"
    local check_heartbeat
    check_heartbeat=$(get_config "select.check_worker_heartbeat" "$NEEDLE_CHECK_WORKER_HEARTBEAT")

    # Skip heartbeat check if disabled
    case "$check_heartbeat" in
        false|False|FALSE|no|No|NO|0) return 0 ;;
    esac

    local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
    local heartbeat_file="$heartbeat_dir/${worker_name}.json"

    # No heartbeat file means worker is not active
    if [[ ! -f "$heartbeat_file" ]]; then
        return 1
    fi

    # Check if the process is still alive
    local pid
    pid=$(jq -r '.pid // 0' "$heartbeat_file" 2>/dev/null)

    if [[ -n "$pid" ]] && [[ "$pid" != "0" ]] && [[ "$pid" != "null" ]]; then
        if kill -0 "$pid" 2>/dev/null; then
            return 0  # Worker is alive
        fi
    fi

    return 1  # Worker is dead or no valid PID
}

# Check if a bead's claim is stale and can be stolen
# Usage: _needle_is_claim_stealable <bead_json>
# Returns: 0 if stealable, 1 if not
_needle_is_claim_stealable() {
    local bead_json="$1"

    # Extract bead fields
    local assignee claim_timestamp updated_at
    assignee=$(echo "$bead_json" | jq -r '.assignee // .claimed_by // empty')
    claim_timestamp=$(echo "$bead_json" | jq -r '.claim_timestamp // empty')
    updated_at=$(echo "$bead_json" | jq -r '.updated_at // empty')

    # Must have an assignee to be stealable
    if [[ -z "$assignee" ]]; then
        return 1
    fi

    # Check 1: Is assignee in the stealable list?
    if _needle_is_stealable_assignee "$assignee"; then
        # Check if claim is old enough
        local claim_time=""
        if [[ -n "$claim_timestamp" ]] && [[ "$claim_timestamp" != "null" ]]; then
            claim_time="$claim_timestamp"
        elif [[ -n "$updated_at" ]] && [[ "$updated_at" != "null" ]]; then
            claim_time="$updated_at"
        fi

        if [[ -n "$claim_time" ]]; then
            # Parse timestamp and check age
            local claim_epoch now age timeout
            claim_epoch=$(_needle_parse_iso8601 "$claim_time")
            now=$(date +%s)
            timeout=$(_needle_get_work_stealing_timeout)

            if [[ -n "$claim_epoch" ]] && [[ "$claim_epoch" -gt 0 ]]; then
                age=$((now - claim_epoch))
                if ((age > timeout)); then
                    _needle_debug "Bead claim is stealable: assignee=$assignee, age=${age}s, timeout=${timeout}s"
                    return 0
                fi
            fi
        fi
    fi

    # Check 2: Does assignee have a heartbeat? (if worker checking enabled)
    local check_heartbeat
    check_heartbeat=$(get_config "select.check_worker_heartbeat" "$NEEDLE_CHECK_WORKER_HEARTBEAT")
    case "$check_heartbeat" in
        true|True|TRUE|yes|Yes|YES|1)
            if ! _needle_worker_has_heartbeat "$assignee"; then
                # No heartbeat - check if it's a stealable assignee or any worker
                if _needle_is_stealable_assignee "$assignee"; then
                    _needle_debug "Bead claim is stealable: assignee=$assignee has no heartbeat"
                    return 0
                fi
                # For non-stealable assignees without heartbeat, still allow stealing
                # This handles dead workers that aren't in the stealable list
                _needle_debug "Bead claim is stealable: assignee=$assignee is dead worker"
                return 0
            fi
            ;;
    esac

    return 1
}

# Parse ISO8601 timestamp to epoch seconds
# Handles formats: 2026-03-03T10:09:09Z, 2026-03-03T10:09:09.123456Z, 2026-03-03T10:09:09+00:00
# Reused from mend.sh for consistency
_needle_parse_iso8601() {
    local timestamp="$1"

    if [[ -z "$timestamp" ]] || [[ "$timestamp" == "null" ]]; then
        echo 0
        return 1
    fi

    local epoch=0

    # Try GNU date first (Linux)
    if epoch=$(date -d "$timestamp" +%s 2>/dev/null) && [[ -n "$epoch" ]] && [[ "$epoch" =~ ^[0-9]+$ ]]; then
        echo "$epoch"
        return 0
    fi

    # Try BSD date (macOS) - strip milliseconds first
    local stripped_ts="${timestamp%%.*}"
    if epoch=$(date -j -f "%Y-%m-%dT%H:%M:%S" "${stripped_ts}" +%s 2>/dev/null) && [[ -n "$epoch" ]] && [[ "$epoch" =~ ^[0-9]+$ ]]; then
        echo "$epoch"
        return 0
    fi

    # Fallback: Parse ISO8601 manually (YYYY-MM-DDTHH:MM:SS)
    if [[ "$timestamp" =~ ^([0-9]{4})-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2}) ]]; then
        local year="${BASH_REMATCH[1]}"
        local month="${BASH_REMATCH[2]}"
        local day="${BASH_REMATCH[3]}"
        local hour="${BASH_REMATCH[4]}"
        local minute="${BASH_REMATCH[5]}"
        local second="${BASH_REMATCH[6]}"

        # Use Python for reliable conversion if available
        if command -v python3 &>/dev/null; then
            epoch=$(python3 -c "
import datetime
try:
    dt = datetime.datetime(${year}, ${month}, ${day}, ${hour}, ${minute}, ${second})
    print(int(dt.timestamp()))
except:
    print(0)
" 2>/dev/null)
            if [[ -n "$epoch" ]] && [[ "$epoch" =~ ^[0-9]+$ ]] && [[ "$epoch" != "0" ]]; then
                echo "$epoch"
                return 0
            fi
        fi
    fi

    echo 0
    return 1
}

# Get claimable beads with fallback for br ready bug
# Usage: _needle_get_claimable_beads [--workspace <path>]
# Returns: JSON array of claimable beads (unassigned, unblocked, not deferred)
# Exit codes:
#   0 - Success (may return empty array)
#   1 - Error
#
# This function implements a workaround for beads_rust v0.1.13 bug where
# br ready fails with "Invalid column type Text at index: 14, name: created_by"
# It falls back to br list with client-side filtering when br ready fails.
_needle_get_claimable_beads() {
    local workspace=""
    local candidates

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --workspace)
                workspace="$2"
                shift 2
                ;;
            *)
                shift
                ;;
        esac
    done

    # DIAGNOSTIC: Log entry with context
    _needle_diag_select "Getting claimable beads" \
        "workspace=$workspace" \
        "session=${NEEDLE_SESSION:-unknown}" \
        "br_available=$(command -v br &>/dev/null && echo 'yes' || echo 'no')"

    _needle_debug "DIAG: _needle_get_claimable_beads called (workspace=${workspace:-current})"

    # Try br ready first (preferred - server-side filtering)
    # Note: br ready outputs to stderr, so we need to capture stderr
    # Also, it may output log lines before JSON, so we extract just the JSON
    local raw_output
    if [[ -n "$workspace" ]]; then
        raw_output=$(br ready --workspace="$workspace" --unassigned --json 2>&1)
    else
        raw_output=$(br ready --unassigned --json 2>&1)
    fi
    local br_exit=$?

    # DIAGNOSTIC: Log br ready call
    _needle_diag_select "br ready call completed" \
        "workspace=$workspace" \
        "exit_code=$br_exit" \
        "output_length=${#raw_output}"

    # Extract JSON portion (from first { or [ to the end)
    candidates=$(echo "$raw_output" | sed -n '/^[{[]/,$p')

    # Check if br ready returned valid JSON array
    if [[ -n "$candidates" ]] && echo "$candidates" | jq -e 'type == "array"' &>/dev/null; then
        local count
        count=$(echo "$candidates" | jq 'length' 2>/dev/null || echo "0")

        _needle_diag_select "br ready returned valid JSON array" \
            "workspace=$workspace" \
            "count=$count" \
            "source=br_ready"

        # FIX: Filter out HUMAN type beads (alerts, not work items)
        # This was previously only done in the fallback path
        local filtered_candidates
        filtered_candidates=$(echo "$candidates" | jq -c '
            [.[] | select(
                .issue_type == null or .issue_type != "human"
            )]
        ' 2>/dev/null)

        local filtered_count
        filtered_count=$(echo "$filtered_candidates" | jq 'length' 2>/dev/null || echo "0")

        if [[ "$filtered_count" -lt "$count" ]]; then
            _needle_debug "Filtered out $((count - filtered_count)) HUMAN type beads from br ready results"
            _needle_diag_select "Filtered HUMAN beads from br ready" \
                "workspace=$workspace" \
                "original_count=$count" \
                "filtered_count=$filtered_count"
        fi

        echo "$filtered_candidates"
        return 0
    fi

    # Check for error response (beads_rust v0.1.13 schema bug)
    if [[ -n "$candidates" ]] && echo "$candidates" | jq -e '.error.code == "DATABASE_ERROR"' &>/dev/null; then
        _needle_debug "DIAG: br ready returned DATABASE_ERROR, using fallback"
        _needle_diag_select "br ready DATABASE_ERROR, using fallback" \
            "workspace=$workspace" \
            "error_type=DATABASE_ERROR"
    elif [[ -z "$candidates" ]]; then
        _needle_debug "DIAG: br ready returned no JSON, using fallback"
        _needle_diag_select "br ready returned no JSON, using fallback" \
            "workspace=$workspace" \
            "error_type=no_json"
    fi

    # br ready failed - use fallback with br list + client-side filtering
    _needle_debug "br ready failed, using br list fallback"

    # Note: br list doesn't support --workspace flag, it operates on current directory
    # Workspace filtering is handled by running in the correct directory
    # FIX: Actually change to the workspace directory if provided
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        _needle_debug "DIAG: Running br list in workspace: $workspace"
        candidates=$(cd "$workspace" && br list --status open --priority 0,1,2,3 --json 2>/dev/null)
    else
        candidates=$(br list --status open --priority 0,1,2,3 --json 2>/dev/null)
    fi
    local list_exit=$?

    _needle_diag_select "br list fallback completed" \
        "workspace=$workspace" \
        "exit_code=$list_exit" \
        "output_length=${#candidates}"

    # Filter client-side: unassigned, unblocked, not deferred, no OPEN dependencies
    # These are the same criteria br ready uses internally
    # NOTE: We used to filter by dependency_count == 0, but that excluded beads
    # whose dependencies were all CLOSED. Now we check if dependencies are actually open.
    # NOTE: Also filter out HUMAN type beads - those are alerts, not work items
    local filtered
    filtered=$(echo "$candidates" | jq -c '
        [.[] | select(
            .assignee == null and
            .blocked_by == null and
            (.deferred_until == null or .deferred_until == "") and
            (.issue_type == null or .issue_type != "human")
        )]
    ' 2>/dev/null)

    local filtered_count
    filtered_count=$(echo "$filtered" | jq 'length' 2>/dev/null || echo "0")

    _needle_diag_select "Initial filtering completed" \
        "workspace=$workspace" \
        "filtered_count=$filtered_count"

    # Now filter out beads with OPEN dependencies
    # For each bead with dependency_count > 0, check if all deps are closed
    local final_filtered="[]"
    while IFS= read -r bead; do
        local bead_id dep_count
        bead_id=$(echo "$bead" | jq -r '.id')
        dep_count=$(echo "$bead" | jq -r '.dependency_count // 0')

        if [[ "$dep_count" -eq 0 ]]; then
            # No dependencies, include it
            final_filtered=$(echo "$final_filtered" | jq -c --argjson bead "$bead" '. + [$bead]')
        else
            # Check if all dependencies are closed
            local deps_status
            deps_status=$(br dep list "$bead_id" --json 2>/dev/null)
            local open_deps
            open_deps=$(echo "$deps_status" | jq '[.[] | select(.status != "closed")] | length' 2>/dev/null || echo "1")

            if [[ "$open_deps" -eq 0 ]]; then
                # All dependencies are closed, include it
                final_filtered=$(echo "$final_filtered" | jq -c --argjson bead "$bead" '. + [$bead]')
                _needle_debug "DIAG: Bead $bead_id has $dep_count deps, all closed - including"
            else
                _needle_debug "DIAG: Bead $bead_id has $open_deps open deps - excluding"
            fi
        fi
    done < <(echo "$filtered" | jq -c '.[]')

    # DIAGNOSTIC: Log the result count
    local count
    count=$(echo "$final_filtered" | jq 'length' 2>/dev/null || echo "0")
    _needle_debug "DIAG: Fallback found $count claimable beads (after checking closed deps)"

    _needle_diag_select "Final claimable beads count" \
        "workspace=$workspace" \
        "count=$count" \
        "source=br_list_fallback"

    echo "$final_filtered"
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

    # Get claimable beads from br CLI (with fallback for br ready bug)
    local candidates
    candidates=$(_needle_get_claimable_beads)

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
    candidates=$(_needle_get_claimable_beads)

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
