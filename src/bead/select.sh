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

# Proactive work stealing defaults
NEEDLE_PROACTIVE_STEALING_ENABLED="${NEEDLE_PROACTIVE_STEALING_ENABLED:-true}"
NEEDLE_STEALING_LOAD_THRESHOLD="${NEEDLE_STEALING_LOAD_THRESHOLD:-2}"
NEEDLE_STEALING_IDLE_THRESHOLD="${NEEDLE_STEALING_IDLE_THRESHOLD:-60}"
NEEDLE_STEAL_FROM_ACTIVE="${NEEDLE_STEAL_FROM_ACTIVE:-false}"

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

# ============================================================================
# Proactive Work Stealing Functions (for idle workers)
# ============================================================================

# Check if proactive work stealing is enabled
# Usage: _needle_proactive_stealing_enabled
# Returns: 0 if enabled, 1 if disabled
_needle_proactive_stealing_enabled() {
    local enabled
    enabled=$(get_config "select.proactive_stealing_enabled" "$NEEDLE_PROACTIVE_STEALING_ENABLED")

    case "$enabled" in
        true|True|TRUE|yes|Yes|YES|1) return 0 ;;
        *) return 1 ;;
    esac
}

# Get the load threshold for considering a worker "overloaded"
# Usage: _needle_get_stealing_load_threshold
# Returns: threshold number (default: 2)
_needle_get_stealing_load_threshold() {
    local threshold
    threshold=$(get_config "select.stealing_load_threshold" "$NEEDLE_STEALING_LOAD_THRESHOLD")

    # Ensure it's a valid number >= 1
    if ! [[ "$threshold" =~ ^[0-9]+$ ]] || [[ "$threshold" -lt 1 ]]; then
        threshold=2
    fi

    echo "$threshold"
}

# Get the idle time threshold before a worker can start stealing
# Usage: _needle_get_stealing_idle_threshold
# Returns: threshold in seconds (default: 60)
_needle_get_stealing_idle_threshold() {
    local threshold
    threshold=$(get_config "select.stealing_idle_threshold" "$NEEDLE_STEALING_IDLE_THRESHOLD")

    # Ensure it's a valid number
    if ! [[ "$threshold" =~ ^[0-9]+$ ]]; then
        threshold=60
    fi

    echo "$threshold"
}

# Check if we can steal from active workers (those with heartbeats)
# Usage: _needle_can_steal_from_active
# Returns: 0 if allowed, 1 if not
_needle_can_steal_from_active() {
    local allowed
    allowed=$(get_config "select.steal_from_active_workers" "$NEEDLE_STEAL_FROM_ACTIVE")

    case "$allowed" in
        true|True|TRUE|yes|Yes|YES|1) return 0 ;;
        *) return 1 ;;
    esac
}

# Get the queue depth (claimed beads count) for a worker
# Usage: _needle_get_worker_queue_depth <worker_name>
# Returns: Number of beads claimed by this worker
_needle_get_worker_queue_depth() {
    local worker_name="$1"
    local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
    local heartbeat_file="$heartbeat_dir/${worker_name}.json"

    # Try to get queue_depth from heartbeat file
    if [[ -f "$heartbeat_file" ]]; then
        local depth
        depth=$(jq -r '.queue_depth // 0' "$heartbeat_file" 2>/dev/null)
        if [[ "$depth" =~ ^[0-9]+$ ]]; then
            echo "$depth"
            return 0
        fi
    fi

    # Fallback: Count beads assigned to this worker in the database
    local count
    count=$(br list --assignee "$worker_name" --status open --json 2>/dev/null | jq 'length' 2>/dev/null || echo "0")
    echo "${count:-0}"
}

# Check if a worker is overloaded (has too many claimed beads)
# Usage: _needle_is_worker_overloaded <worker_name>
# Returns: 0 if overloaded, 1 if not
_needle_is_worker_overloaded() {
    local worker_name="$1"

    # Don't consider ourselves overloaded
    if [[ "$worker_name" == "${NEEDLE_SESSION:-}" ]]; then
        return 1
    fi

    local threshold
    threshold=$(_needle_get_stealing_load_threshold)

    local queue_depth
    queue_depth=$(_needle_get_worker_queue_depth "$worker_name")

    if [[ "$queue_depth" -ge "$threshold" ]]; then
        _needle_debug "Worker $worker_name is overloaded: queue_depth=$queue_depth >= threshold=$threshold"
        return 0
    fi

    return 1
}

# Find all workers that are overloaded and have stealable beads
# Usage: _needle_get_overloaded_workers [--workspace <path>]
# Returns: JSON array of worker info with queue depths
_needle_get_overloaded_workers() {
    local workspace=""
    local threshold
    threshold=$(_needle_get_stealing_load_threshold)

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

    # Get all unique assignees with claimed beads
    local assignees
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        assignees=$(cd "$workspace" && br list --status open --json 2>/dev/null | jq -r '[.[] | select(.assignee != null) | .assignee] | unique | .[]' 2>/dev/null)
    else
        assignees=$(br list --status open --json 2>/dev/null | jq -r '[.[] | select(.assignee != null) | .assignee] | unique | .[]' 2>/dev/null)
    fi

    local overloaded="[]"

    # Check each assignee for overload
    while IFS= read -r assignee; do
        [[ -z "$assignee" || "$assignee" == "null" ]] && continue

        # Skip ourselves
        if [[ "$assignee" == "${NEEDLE_SESSION:-}" ]]; then
            continue
        fi

        local queue_depth
        queue_depth=$(_needle_get_worker_queue_depth "$assignee")

        if [[ "$queue_depth" -ge "$threshold" ]]; then
            # Check if worker is active or if we can steal from active
            local is_active=false
            if _needle_worker_has_heartbeat "$assignee"; then
                is_active=true
            fi

            # Only include if worker is inactive OR we can steal from active
            if [[ "$is_active" == "false" ]] || _needle_can_steal_from_active; then
                overloaded=$(echo "$overloaded" | jq -c \
                    --arg worker "$assignee" \
                    --argjson depth "$queue_depth" \
                    --argjson is_active "$is_active" \
                    '. + [{worker: $worker, queue_depth: $depth, is_active: $is_active}]')
            fi
        fi
    done <<< "$assignees"

    echo "$overloaded"
}

# Find beads that can be stolen from overloaded workers
# Usage: _needle_find_stealable_from_overloaded [--workspace <path>]
# Returns: JSON array of stealable beads from overloaded workers
_needle_find_stealable_from_overloaded() {
    local workspace=""

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

    # Get overloaded workers
    local overloaded
    overloaded=$(_needle_get_overloaded_workers --workspace "$workspace")

    local overloaded_count
    overloaded_count=$(echo "$overloaded" | jq 'length' 2>/dev/null || echo "0")

    if [[ "$overloaded_count" -eq 0 ]]; then
        echo "[]"
        return 0
    fi

    _needle_debug "Found $overloaded_count overloaded worker(s), checking for stealable beads"

    # For each overloaded worker, get their beads and find stealable ones
    local stealable_beads="[]"

    while IFS= read -r worker_info; do
        [[ -z "$worker_info" || "$worker_info" == "null" ]] && continue

        local assignee
        assignee=$(echo "$worker_info" | jq -r '.worker')

        # Get beads assigned to this worker
        local worker_beads
        if [[ -n "$workspace" && -d "$workspace" ]]; then
            worker_beads=$(cd "$workspace" && br list --assignee "$assignee" --status open --json 2>/dev/null)
        else
            worker_beads=$(br list --assignee "$assignee" --status open --json 2>/dev/null)
        fi

        # Filter to find stealable beads
        while IFS= read -r bead; do
            [[ -z "$bead" || "$bead" == "null" ]] && continue

            local bead_id
            bead_id=$(echo "$bead" | jq -r '.id')

            # Check if this bead is stealable (using existing logic)
            if _needle_is_claim_stealable "$bead"; then
                # Also check dependencies
                local dep_count open_deps
                dep_count=$(echo "$bead" | jq -r '.dependency_count // 0')

                if [[ "$dep_count" -gt 0 ]]; then
                    local deps_status
                    deps_status=$(br dep list "$bead_id" --json 2>/dev/null)
                    open_deps=$(echo "$deps_status" | jq '[.[] | select(.status != "closed")] | length' 2>/dev/null || echo "1")
                else
                    open_deps=0
                fi

                if [[ "$open_deps" -eq 0 ]]; then
                    _needle_debug "Found stealable bead $bead_id from overloaded worker $assignee"
                    stealable_beads=$(echo "$stealable_beads" | jq -c --argjson bead "$bead" '. + [$bead]')
                fi
            fi
        done < <(echo "$worker_beads" | jq -c '.[]' 2>/dev/null)
    done < <(echo "$overloaded" | jq -c '.[]')

    local stealable_count
    stealable_count=$(echo "$stealable_beads" | jq 'length' 2>/dev/null || echo "0")

    if [[ "$stealable_count" -gt 0 ]]; then
        _needle_debug "Found $stealable_count stealable bead(s) from overloaded workers"
    fi

    echo "$stealable_beads"
}

# Get priority boost for stolen beads
# Usage: _needle_get_stealing_priority_boost
# Returns: multiplier (default: 1)
_needle_get_stealing_priority_boost() {
    local boost
    boost=$(get_config "select.stealing_priority_boost" "1")

    # Ensure it's a valid number >= 1
    if ! [[ "$boost" =~ ^[0-9]+$ ]] || [[ "$boost" -lt 1 ]]; then
        boost=1
    fi

    echo "$boost"
}

# Check if a bead was stolen from an overloaded worker
# Adds metadata to track work stealing provenance
# Usage: _needle_mark_bead_stolen <bead_id> <from_worker> [workspace]
_needle_mark_bead_stolen() {
    local bead_id="$1"
    local from_worker="$2"
    local workspace="${3:-${NEEDLE_WORKSPACE:-$(pwd)}}"
    local timestamp
    timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)

    _needle_debug "Marking bead $bead_id as stolen from $from_worker"

    # We could add a label or metadata here if br supported it
    # For now, just log it
    _needle_emit_event "bead.stolen" \
        "Bead stolen from overloaded worker" \
        "bead_id=$bead_id" \
        "stolen_from=$from_worker" \
        "stolen_by=${NEEDLE_SESSION:-unknown}" \
        "timestamp=$timestamp"
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
    # NOTE: br ready doesn't support --workspace flag, so we cd to the workspace
    local raw_output
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        raw_output=$(cd "$workspace" && br ready --unassigned --json 2>&1)
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

    # Check if br ready returned valid JSON array and filter in one jq call
    if [[ -n "$candidates" ]]; then
        local filtered_candidates
        filtered_candidates=$(echo "$candidates" | jq -c '
            if type == "array" then
                [.[] | select(.issue_type == null or .issue_type != "human")]
            else
                null
            end
        ' 2>/dev/null)

        if [[ -n "$filtered_candidates" ]] && [[ "$filtered_candidates" != "null" ]]; then

            # ====================================================================
            # WORK STEALING: Also include stealable beads (assigned to inactive workers)
            # ====================================================================
            if _needle_work_stealing_enabled; then
                local all_open_beads
                if [[ -n "$workspace" && -d "$workspace" ]]; then
                    all_open_beads=$(cd "$workspace" && br list --status open --priority-min 0 --priority-max 3 --json 2>/dev/null)
                else
                    all_open_beads=$(br list --status open --priority-min 0 --priority-max 3 --json 2>/dev/null)
                fi

                # Single jq call: filter assigned beads and extract id+assignee+dep_count
                local assigned_data
                assigned_data=$(echo "$all_open_beads" | jq -r '
                    [.[] | select(
                        .assignee != null and
                        .blocked_by == null and
                        (.deferred_until == null or .deferred_until == "") and
                        (.issue_type == null or .issue_type != "human")
                    )] | .[] | [.id, .assignee, (.dependency_count // 0 | tostring)] | @tsv
                ' 2>/dev/null)

                if [[ -n "$assigned_data" ]]; then
                    _needle_debug "DIAG: Checking assigned beads for work stealing (br ready path)"

                    while IFS=$'\t' read -r bead_id assignee dep_count; do
                        [[ -z "$bead_id" || "$bead_id" == "null" ]] && continue

                        # Get full bead JSON for stealability check
                        local bead
                        bead=$(echo "$all_open_beads" | jq -c --arg id "$bead_id" '.[] | select(.id == $id)')

                        if _needle_is_claim_stealable "$bead"; then
                            local open_deps=0
                            if [[ "$dep_count" -gt 0 ]]; then
                                local deps_status
                                deps_status=$(br dep list "$bead_id" --json 2>/dev/null)
                                open_deps=$(echo "$deps_status" | jq '[.[] | select(.status != "closed")] | length' 2>/dev/null || echo "1")
                            fi

                            if [[ "$open_deps" -eq 0 ]]; then
                                _needle_debug "DIAG: Adding stealable bead $bead_id (assignee=$assignee) to br ready results"
                                filtered_candidates=$(echo "$filtered_candidates" | jq -c --argjson bead "$bead" '. + [$bead]')
                            fi
                        fi
                    done <<< "$assigned_data"
                fi
            fi

            # Quick check: return if we have beads (avoid jq length call when possible)
            if [[ "$filtered_candidates" != "[]" ]]; then
                echo "$filtered_candidates"
                return 0
            fi

            # ====================================================================
            # PROACTIVE WORK STEALING (br ready path)
            # ====================================================================
            if _needle_proactive_stealing_enabled; then
                _needle_debug "DIAG: br ready empty, trying proactive work stealing"

                local proactive_beads
                proactive_beads=$(_needle_find_stealable_from_overloaded --workspace "$workspace")

                if [[ -n "$proactive_beads" ]] && [[ "$proactive_beads" != "[]" ]]; then
                    echo "$proactive_beads"
                    return 0
                fi
            fi

            # Empty results - fall through to try fallback path
            _needle_debug "DIAG: br ready returned empty, falling through to fallback path"
        fi
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
    # FIX: Use --priority-min/max instead of comma-separated values (br CLI syntax)
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        _needle_debug "DIAG: Running br list in workspace: $workspace"
        candidates=$(cd "$workspace" && br list --status open --priority-min 0 --priority-max 3 --json 2>/dev/null)
    else
        candidates=$(br list --status open --priority-min 0 --priority-max 3 --json 2>/dev/null)
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

    # ========================================================================
    # WORK STEALING: Include stealable beads assigned to inactive workers
    # ========================================================================
    # This fixes the bug where workers couldn't find work because beads
    # were assigned to "coder" (stealable) but never included in results.
    # See: nd-1sn8 (NEEDLE stuck: no work found)
    if _needle_work_stealing_enabled; then
        local stealable_beads
        # Get beads assigned to stealable assignees (e.g., "coder")
        stealable_beads=$(echo "$candidates" | jq -c '
            [.[] | select(
                .assignee != null and
                .blocked_by == null and
                (.deferred_until == null or .deferred_until == "") and
                (.issue_type == null or .issue_type != "human")
            )]
        ' 2>/dev/null)

        local stealable_count
        stealable_count=$(echo "$stealable_beads" | jq 'length' 2>/dev/null || echo "0")

        if [[ "$stealable_count" -gt 0 ]]; then
            _needle_debug "DIAG: Checking $stealable_count assigned beads for work stealing"

            # Check each assigned bead to see if it's stealable
            while IFS= read -r bead; do
                local bead_id assignee
                bead_id=$(echo "$bead" | jq -r '.id')
                assignee=$(echo "$bead" | jq -r '.assignee')

                # Check if this bead's claim is stealable
                if _needle_is_claim_stealable "$bead"; then
                    # Also check dependencies before including
                    local dep_count open_deps
                    dep_count=$(echo "$bead" | jq -r '.dependency_count // 0')

                    if [[ "$dep_count" -gt 0 ]]; then
                        local deps_status
                        deps_status=$(br dep list "$bead_id" --json 2>/dev/null)
                        open_deps=$(echo "$deps_status" | jq '[.[] | select(.status != "closed")] | length' 2>/dev/null || echo "1")
                    else
                        open_deps=0
                    fi

                    if [[ "$open_deps" -eq 0 ]]; then
                        _needle_debug "DIAG: Adding stealable bead $bead_id (assignee=$assignee)"
                        final_filtered=$(echo "$final_filtered" | jq -c --argjson bead "$bead" '. + [$bead]')
                    fi
                fi
            done < <(echo "$stealable_beads" | jq -c '.[]')
        fi
    fi

    # ========================================================================
    # PROACTIVE WORK STEALING: Steal from overloaded workers
    # ========================================================================
    # If we still have no work, check for overloaded workers we can steal from
    local current_count
    current_count=$(echo "$final_filtered" | jq 'length' 2>/dev/null || echo "0")

    if [[ "$current_count" -eq 0 ]] && _needle_proactive_stealing_enabled; then
        _needle_debug "DIAG: No unassigned beads, checking for overloaded workers"

        local proactive_beads
        proactive_beads=$(_needle_find_stealable_from_overloaded --workspace "$workspace")

        local proactive_count
        proactive_count=$(echo "$proactive_beads" | jq 'length' 2>/dev/null || echo "0")

        if [[ "$proactive_count" -gt 0 ]]; then
            _needle_debug "DIAG: Proactive stealing found $proactive_count bead(s) from overloaded workers"

            # Add proactive stealable beads to our results
            while IFS= read -r bead; do
                [[ -z "$bead" || "$bead" == "null" ]] && continue

                local bead_id assignee
                bead_id=$(echo "$bead" | jq -r '.id')
                assignee=$(echo "$bead" | jq -r '.assignee')

                # Mark as stolen for tracking
                _needle_mark_bead_stolen "$bead_id" "$assignee" "$workspace"

                # Add to results
                final_filtered=$(echo "$final_filtered" | jq -c --argjson bead "$bead" '. + [$bead]')
            done < <(echo "$proactive_beads" | jq -c '.[]')

            _needle_diag_select "Proactive work stealing" \
                "workspace=$workspace" \
                "stolen_count=$proactive_count" \
                "source=overloaded_workers"
        fi
    fi

    # Final count after work stealing
    local final_count
    final_count=$(echo "$final_filtered" | jq 'length' 2>/dev/null || echo "0")

    if [[ "$final_count" -gt "$count" ]]; then
        _needle_debug "DIAG: Work stealing added $((final_count - count)) beads"
        _needle_diag_select "Work stealing added beads" \
            "workspace=$workspace" \
            "unassigned_count=$count" \
            "stolen_count=$((final_count - count))" \
            "total_count=$final_count"
    fi

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
