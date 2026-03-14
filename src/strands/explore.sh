#!/usr/bin/env bash
# NEEDLE Strand: explore (Priority 2)
# Look for work in other workspaces
#
# Implementation: nd-hq2
#
# This strand searches for work in workspaces beyond the configured
# primary workspaces. It expands the search scope when pluck finds nothing.
#
# Key behaviors:
#   - Searches parent directories for .beads directories
#   - Counts unassigned beads in discovered workspaces
#   - Spawns new workers when thresholds are met
#   - Does NOT process beads itself (delegates to spawned workers)
#   - Respects max_depth and max_workers limits
#
# Usage:
#   _needle_strand_explore <workspace> <agent>
#
# Return values:
#   0 - Work was found and processed (should not happen - we spawn, not process)
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

# Get explore threshold - consecutive empty polls before exploring
# Usage: _needle_explore_get_threshold
# Returns: Number of consecutive empty polls (default: 3)
_needle_explore_get_threshold() {
    get_config "strands.explore.threshold" "3" 2>/dev/null
}

# Get spawn threshold - minimum beads needed to spawn a worker
# Usage: _needle_explore_get_spawn_threshold
# Returns: Minimum bead count (default: 3)
_needle_explore_get_spawn_threshold() {
    get_config "scaling.spawn_threshold" "3" 2>/dev/null
}

# Get max workers per agent limit
# Usage: _needle_explore_get_max_workers
# Returns: Maximum workers (default: 10)
_needle_explore_get_max_workers() {
    get_config "scaling.max_workers_per_agent" "10" 2>/dev/null
}

# Get max search depth
# Usage: _needle_explore_get_max_depth
# Returns: Maximum directory depth to search (default: 3)
_needle_explore_get_max_depth() {
    get_config "strands.explore.max_depth" "3" 2>/dev/null
}

# Get cooldown seconds between spawn attempts
# Usage: _needle_explore_get_cooldown
# Returns: Cooldown period in seconds (default: 30)
_needle_explore_get_cooldown() {
    get_config "scaling.cooldown_seconds" "30" 2>/dev/null
}

# ============================================================================
# Workspace Discovery Functions
# ============================================================================

# Find .beads directories in a search path
# Usage: _needle_explore_find_beads_dirs <search_path> <max_depth>
# Returns: Newline-separated list of .beads directory paths
_needle_explore_find_beads_dirs() {
    local search_path="$1"
    local max_depth="${2:-2}"

    # Validate search path exists
    if [[ ! -d "$search_path" ]]; then
        return 0
    fi

    # Find .beads directories, limiting depth
    find "$search_path" -maxdepth "$max_depth" -name ".beads" -type d 2>/dev/null
}

# Count unassigned beads in a workspace
# Uses br CLI to query bead status with --db flag for workspace targeting
#
# Usage: _needle_explore_count_unassigned <workspace>
# Returns: Number of unassigned beads (0 on error)
_needle_explore_count_unassigned() {
    local workspace="$1"

    # Validate workspace has .beads directory
    if [[ ! -d "$workspace/.beads" ]]; then
        echo "0"
        return 0
    fi

    local db_path="$workspace/.beads/beads.db"
    local count

    # Use br ready with --db flag to target the workspace database
    # Count via JSON output and jq length
    count=$(br ready --db="$db_path" --unassigned --json 2>/dev/null | jq 'length' 2>/dev/null)

    # If br ready returned a valid number, use it
    if [[ "$count" =~ ^[0-9]+$ ]]; then
        echo "$count"
        return 0
    fi

    # Fallback: use br list with client-side filtering
    # Count beads that are: status=open, unassigned, unblocked, not deferred
    count=$(br list --db="$db_path" --status open --priority 0,1,2,3 --json 2>/dev/null | \
        jq '[.[] | select(.assignee == null and .blocked_by == null and (.deferred_until == null or .deferred_until == ""))] | length' 2>/dev/null)

    # Handle errors
    if [[ ! "$count" =~ ^[0-9]+$ ]]; then
        echo "0"
        return 0
    fi

    echo "$count"
}

# Count current workers for an agent
# Uses needle list to count active workers
#
# Usage: _needle_explore_count_workers <agent>
# Returns: Number of active workers (0 on error)
_needle_explore_count_workers() {
    local agent="$1"

    # Use needle list with quiet mode and count lines
    local count
    count=$(needle list --agent="$agent" --quiet 2>/dev/null | wc -l)

    # Trim whitespace and validate
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

# Get state file path for spawn cooldown tracking
# Usage: _needle_explore_cooldown_state_file
# Returns: Path to cooldown state file
_needle_explore_cooldown_state_file() {
    echo "$NEEDLE_HOME/$NEEDLE_STATE_DIR/explore_last_spawn.json"
}

# Check if enough time has passed since last spawn (cooldown)
# Usage: _needle_explore_check_cooldown <agent> [<workspace>]
# Returns: 0 if cooldown passed, 1 if still in cooldown
_needle_explore_check_cooldown() {
    local agent="$1"
    local workspace="${2:-global}"

    local cooldown
    cooldown=$(_needle_explore_get_cooldown)

    # If cooldown is 0, always allow
    if [[ "$cooldown" -eq 0 ]]; then
        return 0
    fi

    local state_file
    state_file=$(_needle_explore_cooldown_state_file)

    # Create state file if it doesn't exist
    if [[ ! -f "$state_file" ]]; then
        echo '{}' > "$state_file"
        return 0
    fi

    local now
    now=$(date +%s)

    # Check last spawn time for this agent/workspace combination
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

# Update last spawn time for cooldown tracking
# Usage: _needle_explore_update_cooldown <agent> [<workspace>]
# Returns: 0 on success, 1 on failure
_needle_explore_update_cooldown() {
    local agent="$1"
    local workspace="${2:-global}"

    local state_file
    state_file=$(_needle_explore_cooldown_state_file)

    # Create state file if it doesn't exist
    if [[ ! -f "$state_file" ]]; then
        echo '{}' > "$state_file"
    fi

    local now
    now=$(date +%s)

    local key="${agent}:${workspace}"
    local tmp_file="${state_file}.tmp"

    # Update the last spawn time for this agent/workspace
    if jq --arg k "$key" --arg v "$now" '. + {($k): ($v | tonumber)}' "$state_file" > "$tmp_file" 2>/dev/null; then
        mv "$tmp_file" "$state_file"
        _needle_debug "Updated cooldown state: $key = $now"
        return 0
    else
        _needle_warn "Failed to update cooldown state file"
        return 1
    fi
}

# ============================================================================
# Worker Spawning Functions
# ============================================================================

# Spawn a new worker for a workspace
# Spawns in background and emits event
#
# Usage: _needle_explore_spawn_worker <workspace> <agent>
# Returns: 0 on success, 1 on failure
_needle_explore_spawn_worker() {
    local workspace="$1"
    local agent="$2"

    _needle_debug "Spawning worker for workspace: $workspace, agent: $agent"

    # Get max workers limit
    local max_workers
    max_workers=$(_needle_explore_get_max_workers)

    # Count current workers for this agent
    local current_workers
    current_workers=$(_needle_explore_count_workers "$agent")

    _needle_verbose "Current workers for $agent: $current_workers / $max_workers"

    # Check if we're at the limit
    if (( current_workers >= max_workers )); then
        _needle_debug "At max workers limit ($max_workers), not spawning"
        _needle_emit_event "strand.explore.limit_reached" \
            "Max workers limit reached for agent" \
            "agent=$agent" \
            "current=$current_workers" \
            "max=$max_workers"
        return 1
    fi

    # Check cooldown before spawning
    if ! _needle_explore_check_cooldown "$agent" "$workspace"; then
        _needle_debug "Cooldown active, not spawning worker for $workspace"
        return 1
    fi

    # Spawn worker in background
    # Use nohup to ensure it survives if parent exits
    local spawn_cmd="needle run --workspace=\"$workspace\" --agent=\"$agent\""
    _needle_debug "Spawning: $spawn_cmd"

    # Spawn the worker
    if nohup needle run --workspace="$workspace" --agent="$agent" >/dev/null 2>&1 & then
        local pid=$!

        _needle_info "Spawned worker (PID: $pid) for workspace: $workspace"

        # Update cooldown state
        _needle_explore_update_cooldown "$agent" "$workspace"

        # Emit spawn event
        _needle_emit_event "strand.explore.spawned" \
            "Worker spawned for discovered workspace" \
            "workspace=$workspace" \
            "agent=$agent" \
            "pid=$pid"

        return 0
    else
        _needle_warn "Failed to spawn worker for workspace: $workspace"

        _needle_emit_event "strand.explore.spawn_failed" \
            "Failed to spawn worker" \
            "workspace=$workspace" \
            "agent=$agent"

        return 1
    fi
}

# ============================================================================
# Workspace Search Functions
# ============================================================================

# Search for workspaces with beads using auto-discovery.
# Scans for .beads directories under the discovery root (default: $HOME),
# then spawns workers for any workspace with enough unassigned beads.
#
# Usage: _needle_explore_search_workspaces <primary_workspace> <agent>
# Returns: Number of workspaces found with work
_needle_explore_search_workspaces() {
    local primary_workspace="$1"
    local agent="$2"

    local spawn_threshold
    spawn_threshold=$(_needle_explore_get_spawn_threshold)

    # Use pluck's auto-discovery if available, otherwise fall back to find
    local search_root
    search_root=$(get_config "discovery.root" "$HOME" 2>/dev/null)
    search_root="${search_root/#\~/$HOME}"

    local max_depth
    max_depth=$(get_config "discovery.max_depth" "4" 2>/dev/null)

    _needle_debug "Exploring workspaces: root=$search_root depth=$max_depth spawn_threshold=$spawn_threshold"

    local workspaces_found=0

    # Find all .beads directories
    while IFS= read -r beads_dir; do
        [[ -z "$beads_dir" ]] && continue

        local found_workspace
        found_workspace=$(dirname "$beads_dir")

        # Skip primary workspace (pluck handles that)
        if [[ "$found_workspace" == "$primary_workspace" ]]; then
            continue
        fi

        # Count unassigned beads
        local bead_count
        bead_count=$(_needle_explore_count_unassigned "$found_workspace")

        if (( bead_count > 0 )); then
            _needle_emit_event "strand.explore.found" \
                "Found workspace with available beads" \
                "workspace=$found_workspace" \
                "bead_count=$bead_count"

            ((workspaces_found++))

            if (( bead_count >= spawn_threshold )); then
                _needle_info "Found workspace with $bead_count beads (threshold: $spawn_threshold): $found_workspace"
                _needle_explore_spawn_worker "$found_workspace" "$agent"
            else
                _needle_verbose "Workspace $found_workspace has $bead_count beads, below spawn threshold of $spawn_threshold"
            fi
        fi
    done < <(find "$search_root" -maxdepth "$max_depth" -name ".beads" -type d \
        -not -path "*/node_modules/*" \
        -not -path "*/.git/*" \
        -not -path "*/vendor/*" \
        -not -path "*/.cache/*" 2>/dev/null)

    return $workspaces_found
}

# Legacy alias for backward compatibility
_needle_explore_search_parents() {
    _needle_explore_search_workspaces "$@"
}

# ============================================================================
# Main Strand Entry Point
# ============================================================================

# Main explore strand function
# Searches for work in other workspaces and spawns workers
#
# Usage: _needle_strand_explore <workspace> <agent>
# Arguments:
#   workspace - The primary workspace path
#   agent     - The agent identifier (e.g., "claude-anthropic-sonnet")
#
# Return values:
#   0 - Work was found and processed (should not happen - we spawn, not process)
#   1 - No work found (fallthrough to next strand)
_needle_strand_explore() {
    local workspace="$1"
    local agent="$2"

    _needle_debug "explore strand: searching for work in other workspaces"

    # Validate inputs
    if [[ -z "$workspace" ]]; then
        _needle_error "explore strand: workspace is required"
        return 1
    fi

    if [[ -z "$agent" ]]; then
        _needle_error "explore strand: agent is required"
        return 1
    fi

    # Emit strand started event
    _needle_emit_event "strand.explore.started" \
        "Starting workspace exploration" \
        "primary_workspace=$workspace" \
        "agent=$agent"

    # Search for workspaces with beads via auto-discovery
    local workspaces_found=0
    _needle_explore_search_workspaces "$workspace" "$agent"
    workspaces_found=$?

    if (( workspaces_found > 0 )); then
        _needle_debug "explore strand: found $workspaces_found workspace(s) with work"

        _needle_emit_event "strand.explore.completed" \
            "Exploration completed, workspaces found" \
            "workspaces_found=$workspaces_found" \
            "workers_spawned=true"

        # Note: We return 1 (no work found) because we spawn workers
        # rather than processing beads ourselves. The spawned workers
        # will pick up the work in subsequent cycles.
        #
        # This is intentional - explore's job is to find work and
        # delegate, not to process it directly.
        _needle_verbose "explore strand: spawned workers, falling through"
    else
        _needle_debug "explore strand: no workspaces with work found"

        _needle_emit_event "strand.explore.completed" \
            "Exploration completed, no workspaces found" \
            "workspaces_found=0" \
            "workers_spawned=false"
    fi

    # Always return 1 (no work found) - we spawn workers, not process beads
    # This allows the strand waterfall to continue to mend, weave, etc.
    return 1
}

# ============================================================================
# Utility Functions
# ============================================================================

# NOTE: _needle_explore_is_enabled removed — strand enablement is now
# controlled by presence in the config strand list

# Get statistics about the explore strand
# Usage: _needle_explore_stats
# Returns: JSON object with stats
_needle_explore_stats() {
    local threshold spawn_threshold max_workers max_depth cooldown
    threshold=$(_needle_explore_get_threshold)
    spawn_threshold=$(_needle_explore_get_spawn_threshold)
    max_workers=$(_needle_explore_get_max_workers)
    max_depth=$(_needle_explore_get_max_depth)
    cooldown=$(_needle_explore_get_cooldown)

    _needle_json_object \
        "strand=explore" \
        "priority=2" \
        "explore_threshold=$threshold" \
        "spawn_threshold=$spawn_threshold" \
        "max_workers=$max_workers" \
        "max_depth=$max_depth" \
        "cooldown_seconds=$cooldown"
}

# ============================================================================
# Direct Execution Support (for testing)
# ============================================================================

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    # Load required modules for standalone execution
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
        search)
            if [[ $# -lt 3 ]]; then
                echo "Usage: $0 search <primary_workspace> <agent>"
                exit 1
            fi
            _needle_explore_search_parents "$2" "$3"
            echo "Workspaces found: $?"
            ;;
        count)
            if [[ $# -lt 2 ]]; then
                echo "Usage: $0 count <workspace>"
                exit 1
            fi
            echo "Unassigned beads: $(_needle_explore_count_unassigned "$2")"
            ;;
        spawn)
            if [[ $# -lt 3 ]]; then
                echo "Usage: $0 spawn <workspace> <agent>"
                exit 1
            fi
            _needle_explore_spawn_worker "$2" "$3"
            exit $?
            ;;
        stats)
            _needle_explore_stats | jq .
            ;;
        config)
            echo "Explore Configuration:"
            echo "  explore_threshold: $(_needle_explore_get_threshold)"
            echo "  spawn_threshold: $(_needle_explore_get_spawn_threshold)"
            echo "  max_workers: $(_needle_explore_get_max_workers)"
            echo "  cooldown_seconds: $(_needle_explore_get_cooldown)"
            echo "  max_depth: $(_needle_explore_get_max_depth)"
            ;;
        -h|--help)
            echo "Usage: $0 <command> [args]"
            echo ""
            echo "Commands:"
            echo "  run <workspace> <agent>      Run the explore strand"
            echo "  search <workspace> <agent>   Search for workspaces with work"
            echo "  count <workspace>            Count unassigned beads in workspace"
            echo "  spawn <workspace> <agent>    Spawn a worker for workspace"
            echo "  stats                        Show strand statistics"
            echo "  config                       Show current configuration"
            echo ""
            echo "The explore strand:"
            echo "  1. Searches parent directories for .beads directories"
            echo "  2. Counts unassigned beads in discovered workspaces"
            echo "  3. Spawns workers when bead count meets threshold"
            echo "  4. Does NOT process beads directly (delegates to spawned workers)"
            ;;
        *)
            echo "Unknown command: ${1:-}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
fi
