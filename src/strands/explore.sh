#!/usr/bin/env bash
# NEEDLE Strand: explore (Priority 3)
# Search for work in child and sibling workspaces
#
# Implementation: nd-hq2
#
# This strand expands the search scope when pluck and mend find nothing.
# It searches in two phases:
#   Phase 1 (Down): Search child directories for .beads/ workspaces
#   Phase 2 (Up):   Walk up parent directories, searching siblings at each level
#
# At each discovered workspace, explore checks for:
#   - Open, claimable beads (spawns workers or claims directly)
#   - Stale in_progress beads (dead workers holding claims)
#
# The upward walk is constrained by explore.max_upward_depth config (default: 3).
#
# Usage:
#   _needle_strand_explore <workspace> <agent>
#
# Return values:
#   0 - Work was found (workers spawned or beads reclaimed)
#   1 - No work found (fallthrough to next strand)

# Source dependencies (if not already loaded)
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

if [[ -z "${_NEEDLE_CONFIG_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/config.sh"
fi

if [[ -z "${_NEEDLE_JSON_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/json.sh"
fi

# ============================================================================
# Configuration Helpers
# ============================================================================

_needle_explore_get_threshold() {
    get_config "strands.explore.threshold" "3" 2>/dev/null
}

_needle_explore_get_spawn_threshold() {
    get_config "scaling.spawn_threshold" "3" 2>/dev/null
}

_needle_explore_get_max_workers() {
    get_config "scaling.max_workers_per_agent" "10" 2>/dev/null
}

# Max depth when searching downward into children
_needle_explore_get_max_depth() {
    get_config "strands.explore.max_depth" "3" 2>/dev/null
}

# Max levels to walk upward from the workspace
_needle_explore_get_max_upward_depth() {
    get_config "strands.explore.max_upward_depth" "3" 2>/dev/null
}

_needle_explore_get_cooldown() {
    get_config "scaling.cooldown_seconds" "30" 2>/dev/null
}

# ============================================================================
# Workspace Discovery Functions
# ============================================================================

# Count unassigned beads in a workspace
_needle_explore_count_unassigned() {
    local workspace="$1"

    if [[ ! -d "$workspace/.beads" ]]; then
        echo "0"
        return 0
    fi

    local count

    # Use br ready by cd-ing into the workspace (same as _needle_get_claimable_beads).
    # This correctly handles dependency-blocked beads and reads the WAL for the latest
    # state — unlike br ready --db which can miss WAL updates and fall back to a broken
    # br list filter that counts blocked beads as claimable.
    count=$(cd "$workspace" && br ready --unassigned --json 2>/dev/null | jq 'length' 2>/dev/null)

    if [[ "$count" =~ ^[0-9]+$ ]]; then
        echo "$count"
        return 0
    fi

    # Fallback: JSONL-only mode — still respects dependency blocks
    count=$(cd "$workspace" && br ready --no-db --unassigned --json 2>/dev/null | jq 'length' 2>/dev/null)

    if [[ ! "$count" =~ ^[0-9]+$ ]]; then
        echo "0"
        return 0
    fi

    echo "$count"
}

# Check for stale in_progress beads in a workspace (dead workers holding claims)
# Returns: number of stale beads found and released
_needle_explore_check_stale() {
    local workspace="$1"
    local released=0

    if [[ ! -d "$workspace/.beads" ]]; then
        echo "0"
        return 0
    fi

    local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
    local db_path="$workspace/.beads/beads.db"

    # Get in_progress beads
    local in_progress
    in_progress=$(br list --db="$db_path" --status in_progress --json 2>/dev/null)

    if [[ -z "$in_progress" ]] || [[ "$in_progress" == "[]" ]] || [[ "$in_progress" == "null" ]]; then
        echo "0"
        return 0
    fi

    # Check each in_progress bead's assignee for liveness
    while IFS= read -r bead; do
        local bead_id assignee
        bead_id=$(echo "$bead" | jq -r '.id // empty')
        assignee=$(echo "$bead" | jq -r '.assignee // empty')

        [[ -z "$bead_id" ]] && continue
        [[ -z "$assignee" ]] && continue

        # Check if assignee worker is alive
        local worker_alive=false
        local hb_file="$heartbeat_dir/${assignee}.json"

        if [[ -f "$hb_file" ]]; then
            local pid
            pid=$(jq -r '.pid // 0' "$hb_file" 2>/dev/null)
            if [[ -n "$pid" ]] && [[ "$pid" != "0" ]] && [[ "$pid" != "null" ]]; then
                if kill -0 "$pid" 2>/dev/null; then
                    worker_alive=true
                fi
            fi
        fi

        if [[ "$worker_alive" == "false" ]]; then
            _needle_warn "explore: stale bead $bead_id in $workspace (worker $assignee dead)"

            # Release via br update --db
            local ws_db="$workspace/.beads/beads.db"
            if [[ -f "$ws_db" ]] && br update "$bead_id" --status open --assignee "" --db="$ws_db" --lock-timeout 5000 2>/dev/null; then
                _needle_info "explore: released stale bead $bead_id"
                ((released++))
            elif declare -f _needle_mend_release_bead &>/dev/null; then
                if _needle_mend_release_bead "$ws_db" "$bead_id" "explore_stale_reclaim"; then
                    ((released++))
                fi
            fi
        fi
    done < <(echo "$in_progress" | jq -c '.[]' 2>/dev/null)

    echo "$released"
}

# Count current workers for an agent
_needle_explore_count_workers() {
    local agent="$1"
    local count
    count=$(needle list --agent="$agent" --quiet 2>/dev/null | wc -l)
    count="${count//[[:space:]]/}"
    if [[ ! "$count" =~ ^[0-9]+$ ]]; then
        echo "0"
        return 0
    fi
    echo "$count"
}

# ============================================================================
# Cooldown State Management
# ============================================================================

_needle_explore_cooldown_state_file() {
    echo "$NEEDLE_HOME/$NEEDLE_STATE_DIR/explore_last_spawn.json"
}

_needle_explore_check_cooldown() {
    local agent="$1"
    local workspace="${2:-global}"

    local cooldown
    cooldown=$(_needle_explore_get_cooldown)

    if [[ "$cooldown" -eq 0 ]]; then
        return 0
    fi

    local state_file
    state_file=$(_needle_explore_cooldown_state_file)

    if [[ ! -f "$state_file" ]]; then
        echo '{}' > "$state_file"
        return 0
    fi

    local now
    now=$(date +%s)

    local key="${agent}:${workspace}"
    local last_spawn
    last_spawn=$(jq -r --arg k "$key" '.[$k] // "0"' "$state_file" 2>/dev/null)

    if [[ -z "$last_spawn" ]] || [[ "$last_spawn" == "0" ]]; then
        return 0
    fi

    local elapsed=$((now - last_spawn))

    if [[ "$elapsed" -lt "$cooldown" ]]; then
        _needle_debug "Cooldown active: ${elapsed}s elapsed, need ${cooldown}s (agent: $agent, workspace: $workspace)"
        return 1
    fi

    return 0
}

_needle_explore_update_cooldown() {
    local agent="$1"
    local workspace="${2:-global}"

    local state_file
    state_file=$(_needle_explore_cooldown_state_file)

    if [[ ! -f "$state_file" ]]; then
        echo '{}' > "$state_file"
    fi

    local now
    now=$(date +%s)

    local key="${agent}:${workspace}"
    local tmp_file="${state_file}.tmp"

    if jq --arg k "$key" --arg v "$now" '. + {($k): ($v | tonumber)}' "$state_file" > "$tmp_file" 2>/dev/null; then
        mv "$tmp_file" "$state_file"
        return 0
    else
        _needle_warn "Failed to update cooldown state file"
        return 1
    fi
}

# ============================================================================
# Worker Spawning
# ============================================================================

_needle_explore_spawn_worker() {
    local workspace="$1"
    local agent="$2"

    _needle_debug "Spawning worker for workspace: $workspace, agent: $agent"

    local max_workers
    max_workers=$(_needle_explore_get_max_workers)

    local current_workers
    current_workers=$(_needle_explore_count_workers "$agent")

    _needle_verbose "Current workers for $agent: $current_workers / $max_workers"

    if (( current_workers >= max_workers )); then
        _needle_debug "At max workers limit ($max_workers), not spawning"
        return 1
    fi

    if ! _needle_explore_check_cooldown "$agent" "$workspace"; then
        _needle_debug "Cooldown active, not spawning worker for $workspace"
        return 1
    fi

    if nohup needle run --workspace="$workspace" --agent="$agent" >/dev/null 2>&1 & then
        local pid=$!
        _needle_info "Spawned worker (PID: $pid) for workspace: $workspace"
        _needle_explore_update_cooldown "$agent" "$workspace"

        _needle_telemetry_emit "explore.worker_spawned" "info" \
            "workspace=$workspace" \
            "agent=$agent" \
            "pid=$pid"

        return 0
    else
        _needle_warn "Failed to spawn worker for workspace: $workspace"
        return 1
    fi
}

# Spawn workers based on spawn_threshold for a workspace with beads.
# If unassigned bead count exceeds spawn_threshold, spawn additional workers.
# Respects max_workers_per_agent and cooldown_seconds.
#
# Usage: _needle_explore_spawn_workers_if_needed <workspace> <agent>
# Returns: 0 on success, 1 on failure
_needle_explore_spawn_workers_if_needed() {
    local workspace="$1"
    local agent="$2"

    # Check cooldown first before any calculations
    if ! _needle_explore_check_cooldown "$agent" "$workspace"; then
        _needle_debug "Auto-scaling: cooldown active, not spawning workers for $workspace"
        return 1
    fi

    local spawn_threshold
    spawn_threshold=$(_needle_explore_get_spawn_threshold)

    # Get unassigned bead count
    local bead_count
    bead_count=$(_needle_explore_count_unassigned "$workspace")

    if [[ "$bead_count" -le "$spawn_threshold" ]]; then
        _needle_debug "Auto-scaling: bead count ($bead_count) does not exceed spawn_threshold ($spawn_threshold), not spawning"
        return 0
    fi

    # Get max_workers and current workers
    local max_workers
    max_workers=$(_needle_explore_get_max_workers)

    local current_workers
    current_workers=$(_needle_explore_count_workers "$agent")

    # Calculate how many workers we can spawn
    local available_slots=$((max_workers - current_workers))

    if [[ "$available_slots" -le 0 ]]; then
        _needle_debug "Auto-scaling: at max workers limit ($max_workers), cannot spawn more"
        return 1
    fi

    # Calculate how many workers to spawn based on bead count
    # Spawn enough workers so each worker handles spawn_threshold beads
    local workers_to_spawn=$(( (bead_count - spawn_threshold + spawn_threshold - 1) / spawn_threshold ))

    # But don't exceed available slots
    if [[ "$workers_to_spawn" -gt "$available_slots" ]]; then
        workers_to_spawn="$available_slots"
    fi

    if [[ "$workers_to_spawn" -le 0 ]]; then
        return 0
    fi

    _needle_info "Auto-scaling: spawning $workers_to_spawn worker(s) for $workspace (bead_count=$bead_count, threshold=$spawn_threshold)"

    # Spawn workers in batch (cooldown already checked above)
    local spawned=0
    local i
    for ((i = 0; i < workers_to_spawn; i++)); do
        # Check max_workers on each iteration (in case something changed)
        current_workers=$(_needle_explore_count_workers "$agent")
        if (( current_workers >= max_workers )); then
            _needle_debug "Auto-scaling: reached max workers ($max_workers), stopping spawn batch"
            break
        fi

        if nohup needle run --workspace="$workspace" --agent="$agent" >/dev/null 2>&1 & then
            local pid=$!
            ((spawned++))
            _needle_verbose "Auto-scaling: spawned worker (PID: $pid) for $workspace"
        else
            _needle_warn "Auto-scaling: failed to spawn worker $((i + 1)) for $workspace"
        fi
    done

    # Update cooldown after spawn batch (not per-worker)
    if [[ "$spawned" -gt 0 ]]; then
        _needle_explore_update_cooldown "$agent" "$workspace"

        _needle_telemetry_emit "explore.auto_scaling" "info" \
            "workspace=$workspace" \
            "agent=$agent" \
            "bead_count=$bead_count" \
            "spawn_threshold=$spawn_threshold" \
            "workers_spawned=$spawned"
        return 0
    else
        _needle_warn "Auto-scaling: failed to spawn any workers for $workspace"
        return 1
    fi
}

# ============================================================================
# Phase 1: Search Children (Downward)
# ============================================================================

# Find the first child workspace that has claimable beads.
# Returns: workspace path on stdout (empty if none found)
_needle_explore_find_child_with_beads() {
    local workspace="$1"
    local max_depth="$2"

    _needle_debug "explore: searching children of $workspace (depth=$max_depth)"

    while IFS= read -r beads_dir; do
        [[ -z "$beads_dir" ]] && continue

        local found_workspace
        found_workspace=$(dirname "$beads_dir")

        # Skip self
        [[ "$found_workspace" == "$workspace" ]] && continue

        local bead_count
        bead_count=$(_needle_explore_count_unassigned "$found_workspace")

        if (( bead_count > 0 )); then
            _needle_debug "explore: child $found_workspace has $bead_count claimable bead(s)"
            echo "$found_workspace"
            return 0
        fi
    done < <(find "$workspace" -maxdepth "$max_depth" -name ".beads" -type d \
        -not -path "*/node_modules/*" \
        -not -path "*/.git/*" \
        -not -path "*/vendor/*" \
        -not -path "*/.cache/*" 2>/dev/null)

    return 1
}

# Check for stale beads in all child workspaces and release them.
# Returns via stdout: total number of stale beads released
_needle_explore_search_children_stale() {
    local workspace="$1"
    local max_depth="$2"
    local total_released=0

    while IFS= read -r beads_dir; do
        [[ -z "$beads_dir" ]] && continue

        local found_workspace
        found_workspace=$(dirname "$beads_dir")

        [[ "$found_workspace" == "$workspace" ]] && continue

        local released
        released=$(_needle_explore_check_stale "$found_workspace")
        total_released=$((total_released + released))
    done < <(find "$workspace" -maxdepth "$max_depth" -name ".beads" -type d \
        -not -path "*/node_modules/*" \
        -not -path "*/.git/*" \
        -not -path "*/vendor/*" \
        -not -path "*/.cache/*" 2>/dev/null)

    echo "$total_released"
}

# ============================================================================
# Phase 2: Walk Upward (Siblings at each level)
# ============================================================================

# Walk up from the workspace, searching siblings at each level.
# Returns: first sibling workspace path with claimable beads (empty if none)
_needle_explore_find_sibling_with_beads() {
    local workspace="$1"
    local max_upward="$2"
    local child_depth="$3"

    local current="$workspace"
    local level=0

    _needle_debug "explore: walking upward from $workspace (max_upward=$max_upward)"

    while (( level < max_upward )); do
        local parent
        parent=$(dirname "$current")

        # Stop at filesystem root
        if [[ "$parent" == "$current" ]] || [[ "$parent" == "/" ]]; then
            break
        fi

        ((level++))
        _needle_debug "explore: checking siblings at level $level ($parent)"

        while IFS= read -r beads_dir; do
            [[ -z "$beads_dir" ]] && continue

            local found_workspace
            found_workspace=$(dirname "$beads_dir")

            # Skip the original workspace
            [[ "$found_workspace" == "$workspace" ]] && continue

            local bead_count
            bead_count=$(_needle_explore_count_unassigned "$found_workspace")

            if (( bead_count > 0 )); then
                _needle_debug "explore: sibling $found_workspace has $bead_count claimable bead(s) (level $level)"
                echo "$found_workspace"
                return 0
            fi

            # Also check for stale beads while we're here
            local released
            released=$(_needle_explore_check_stale "$found_workspace")
            if (( released > 0 )); then
                _needle_info "explore: released $released stale bead(s) in sibling $found_workspace"
                # Re-check if this workspace now has claimable beads
                bead_count=$(_needle_explore_count_unassigned "$found_workspace")
                if (( bead_count > 0 )); then
                    echo "$found_workspace"
                    return 0
                fi
            fi
        done < <(find "$parent" -maxdepth "$child_depth" -name ".beads" -type d \
            -not -path "*/node_modules/*" \
            -not -path "*/.git/*" \
            -not -path "*/vendor/*" \
            -not -path "*/.cache/*" 2>/dev/null)

        current="$parent"
    done

    return 1
}

# ============================================================================
# Main Strand Entry Point
# ============================================================================

_needle_strand_explore() {
    local workspace="$1"
    local agent="$2"

    _needle_debug "explore strand: searching for work beyond $workspace"

    if [[ -z "$workspace" ]] || [[ -z "$agent" ]]; then
        _needle_error "explore strand: workspace and agent are required"
        return 1
    fi

    local max_depth
    max_depth=$(_needle_explore_get_max_depth)

    local max_upward
    max_upward=$(_needle_explore_get_max_upward_depth)

    # Phase 1: Search children of current workspace
    # If a child workspace has beads, signal the engine to change workspace
    # and restart from pluck (return 2).
    local child_workspace
    child_workspace=$(_needle_explore_find_child_with_beads "$workspace" "$max_depth")

    if [[ -n "$child_workspace" ]]; then
        _needle_info "explore: child workspace $child_workspace has beads, switching"

        # Auto-scale: spawn additional workers if bead count exceeds threshold
        _needle_explore_spawn_workers_if_needed "$child_workspace" "$agent"

        _needle_telemetry_emit "explore.workspace_switch" "info" \
            "from=$workspace" \
            "to=$child_workspace" \
            "direction=down"

        # Signal the engine to change workspace and restart
        export NEEDLE_EXPLORE_NEW_WORKSPACE="$child_workspace"
        return 2
    fi

    # Also check stale beads in children (dead workers holding claims)
    local children_stale
    children_stale=$(_needle_explore_search_children_stale "$workspace" "$max_depth")

    if (( children_stale > 0 )); then
        _needle_info "explore: released $children_stale stale bead(s) in children"
        # Don't return 0 here — the freed beads are in other workspaces.
        # Instead, re-check if any child now has claimable beads
        child_workspace=$(_needle_explore_find_child_with_beads "$workspace" "$max_depth")
        if [[ -n "$child_workspace" ]]; then
            export NEEDLE_EXPLORE_NEW_WORKSPACE="$child_workspace"
            return 2
        fi
    fi

    # Phase 2: Walk upward, checking siblings at each level
    # If a sibling workspace has beads, change workspace and restart
    local sibling_workspace
    sibling_workspace=$(_needle_explore_find_sibling_with_beads "$workspace" "$max_upward" "$max_depth")

    if [[ -n "$sibling_workspace" ]]; then
        _needle_info "explore: sibling workspace $sibling_workspace has beads, switching"

        # Auto-scale: spawn additional workers if bead count exceeds threshold
        _needle_explore_spawn_workers_if_needed "$sibling_workspace" "$agent"

        _needle_telemetry_emit "explore.workspace_switch" "info" \
            "from=$workspace" \
            "to=$sibling_workspace" \
            "direction=up"

        export NEEDLE_EXPLORE_NEW_WORKSPACE="$sibling_workspace"
        return 2
    fi

    _needle_telemetry_emit "explore.scan_completed" "info" \
        "workspace=$workspace" \
        "result=no_workspaces_found"

    _needle_debug "explore: no workspaces with work found"
    return 1
}

# ============================================================================
# Utility Functions
# ============================================================================

_needle_explore_stats() {
    local threshold spawn_threshold max_workers max_depth max_upward cooldown
    threshold=$(_needle_explore_get_threshold)
    spawn_threshold=$(_needle_explore_get_spawn_threshold)
    max_workers=$(_needle_explore_get_max_workers)
    max_depth=$(_needle_explore_get_max_depth)
    max_upward=$(_needle_explore_get_max_upward_depth)
    cooldown=$(_needle_explore_get_cooldown)

    _needle_json_object \
        "strand=explore" \
        "priority=3" \
        "explore_threshold=$threshold" \
        "spawn_threshold=$spawn_threshold" \
        "max_workers=$max_workers" \
        "max_child_depth=$max_depth" \
        "max_upward_depth=$max_upward" \
        "cooldown_seconds=$cooldown"
}

# Legacy alias
_needle_explore_search_parents() {
    _needle_explore_search_children "$@"
}

# ============================================================================
# Direct Execution Support (for testing)
# ============================================================================

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    NEEDLE_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
    source "$NEEDLE_SRC/lib/output.sh"
    source "$NEEDLE_SRC/lib/config.sh"
    source "$NEEDLE_SRC/lib/json.sh"
    source "$NEEDLE_SRC/lib/paths.sh"

    case "${1:-}" in
        run)
            if [[ $# -lt 3 ]]; then
                echo "Usage: $0 run <workspace> <agent>"
                exit 1
            fi
            _needle_strand_explore "$2" "$3"
            exit $?
            ;;
        stats)
            _needle_explore_stats | jq .
            ;;
        -h|--help)
            echo "Usage: $0 <command> [args]"
            echo ""
            echo "Commands:"
            echo "  run <workspace> <agent>   Run the explore strand"
            echo "  stats                     Show strand statistics"
            echo ""
            echo "The explore strand searches in two phases:"
            echo "  Phase 1 (Down): Search child directories for .beads/"
            echo "  Phase 2 (Up):   Walk up, searching siblings at each level"
            echo ""
            echo "At each workspace found, checks for:"
            echo "  - Open, claimable beads (spawns workers)"
            echo "  - Stale in_progress beads (reclaims from dead workers)"
            ;;
        *)
            echo "Unknown command: ${1:-}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
fi
