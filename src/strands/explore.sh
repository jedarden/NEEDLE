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
# Uses br CLI to query bead status with fallback for br ready bug
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

    # Try br ready first
    local count
    count=$(br ready --workspace="$workspace" --unassigned --count 2>/dev/null)

    # If br ready returned a valid number, use it
    if [[ "$count" =~ ^[0-9]+$ ]]; then
        echo "$count"
        return 0
    fi

    # Fallback: use br list with client-side filtering
    # Count beads that are: status=open, unassigned, unblocked, not deferred
    count=$(br list --workspace="$workspace" --status open --priority 0,1,2,3 --json 2>/dev/null | \
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

    # Spawn worker in background
    # Use nohup to ensure it survives if parent exits
    local spawn_cmd="needle run --workspace=\"$workspace\" --agent=\"$agent\""
    _needle_debug "Spawning: $spawn_cmd"

    # Spawn the worker
    if nohup needle run --workspace="$workspace" --agent="$agent" >/dev/null 2>&1 & then
        local pid=$!

        _needle_info "Spawned worker (PID: $pid) for workspace: $workspace"

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

# Search parent directories for workspaces with work
# Traverses up the directory tree looking for .beads directories
#
# Usage: _needle_explore_search_parents <primary_workspace> <agent>
# Returns: Number of workspaces found with work
_needle_explore_search_parents() {
    local primary_workspace="$1"
    local agent="$2"

    local max_depth
    max_depth=$(_needle_explore_get_max_depth)

    local spawn_threshold
    spawn_threshold=$(_needle_explore_get_spawn_threshold)

    _needle_debug "Searching for workspaces (max_depth: $max_depth, spawn_threshold: $spawn_threshold)"

    # Start from parent of primary workspace
    local search_dir
    search_dir=$(dirname "$primary_workspace")

    local depth=0
    local workspaces_found=0

    # Traverse up the directory tree
    while [[ "$search_dir" != "/" && $depth -lt $max_depth ]]; do
        _needle_verbose "Searching at depth $depth: $search_dir"

        # Find .beads directories at this level
        local beads_dirs
        beads_dirs=$(_needle_explore_find_beads_dirs "$search_dir" 2)

        # Process each found .beads directory
        while IFS= read -r beads_dir; do
            [[ -z "$beads_dir" ]] && continue

            # Get workspace path (parent of .beads)
            local found_workspace
            found_workspace=$(dirname "$beads_dir")

            # Skip if same as primary workspace
            if [[ "$found_workspace" == "$primary_workspace" ]]; then
                _needle_verbose "Skipping primary workspace: $found_workspace"
                continue
            fi

            # Count unassigned beads
            local bead_count
            bead_count=$(_needle_explore_count_unassigned "$found_workspace")

            _needle_verbose "Workspace $found_workspace has $bead_count unassigned beads"

            # Check if there's enough work to spawn
            if (( bead_count > 0 )); then
                # Emit discovery event
                _needle_emit_event "strand.explore.found" \
                    "Found workspace with available beads" \
                    "workspace=$found_workspace" \
                    "bead_count=$bead_count" \
                    "depth=$depth"

                ((workspaces_found++))

                # Spawn worker if threshold met
                if (( bead_count >= spawn_threshold )); then
                    _needle_info "Found workspace with $bead_count beads (threshold: $spawn_threshold): $found_workspace"
                    _needle_explore_spawn_worker "$found_workspace" "$agent"
                else
                    _needle_verbose "Workspace has $bead_count beads, below spawn threshold of $spawn_threshold"
                fi
            fi
        done <<< "$beads_dirs"

        # Move up one directory
        search_dir=$(dirname "$search_dir")
        ((depth++))
    done

    return $workspaces_found
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

    # Search parent directories for workspaces with work
    local workspaces_found=0
    _needle_explore_search_parents "$workspace" "$agent"
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

# Check if explore strand is enabled
# Usage: _needle_explore_is_enabled
# Returns: 0 if enabled, 1 if disabled
_needle_explore_is_enabled() {
    local enabled
    enabled=$(get_config "strands.explore" "true" 2>/dev/null)

    case "$enabled" in
        true|True|TRUE|yes|Yes|YES|1)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

# Get statistics about the explore strand
# Usage: _needle_explore_stats
# Returns: JSON object with stats
_needle_explore_stats() {
    local threshold spawn_threshold max_workers max_depth
    threshold=$(_needle_explore_get_threshold)
    spawn_threshold=$(_needle_explore_get_spawn_threshold)
    max_workers=$(_needle_explore_get_max_workers)
    max_depth=$(_needle_explore_get_max_depth)

    _needle_json_object \
        "strand=explore" \
        "priority=2" \
        "explore_threshold=$threshold" \
        "spawn_threshold=$spawn_threshold" \
        "max_workers=$max_workers" \
        "max_depth=$max_depth"
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
