#!/usr/bin/env bash
# NEEDLE Strand Engine - Dispatcher for the 7-strand priority waterfall
#
# The strand engine implements the priority waterfall, dispatching work
# through the 7-strand system in order. It SOURCES strand implementations
# but does NOT implement the strands themselves.
#
# Strand order (priority waterfall):
#   1. pluck   - Primary work from configured workspaces (nd-2gc)
#   2. explore - Look for work in other workspaces (nd-hq2)
#   3. mend    - Maintenance and cleanup (nd-1sk)
#   4. weave   - Create beads from documentation gaps (nd-27u)
#   5. unravel - Create alternatives for blocked beads (nd-20p)
#   6. pulse   - Codebase health monitoring (nd-qpj)
#   7. knot    - Alert human when stuck (nd-d2a)
#
# Usage:
#   source "$NEEDLE_SRC/strands/engine.sh"
#   _needle_strand_engine "$workspace" "$agent"
#
# Return values:
#   0 - Work was found and processed
#   1 - No work found across all strands

# Get NEEDLE_SRC if not already set
if [[ -z "${NEEDLE_SRC:-}" ]]; then
    NEEDLE_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi

# Source individual strand implementations
# Each strand file defines _needle_strand_<name>() function
source "$NEEDLE_SRC/strands/pluck.sh"
source "$NEEDLE_SRC/strands/explore.sh"
source "$NEEDLE_SRC/strands/mend.sh"
source "$NEEDLE_SRC/strands/weave.sh"
source "$NEEDLE_SRC/strands/unravel.sh"
source "$NEEDLE_SRC/strands/pulse.sh"
source "$NEEDLE_SRC/strands/knot.sh"

# ============================================================================
# Strand Enable/Disable Check
# ============================================================================

# Check if a strand is enabled in configuration
# Usage: _needle_is_strand_enabled <strand_name>
# Returns: 0 if enabled, 1 if disabled
# Example: _needle_is_strand_enabled "pluck"
_needle_is_strand_enabled() {
    local strand="$1"

    # Default to false if not configured
    local enabled
    enabled="$(get_config "strands.$strand" "false" 2>/dev/null)"

    # Handle various true representations
    case "$enabled" in
        true|True|TRUE|yes|Yes|YES|1)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

# ============================================================================
# Strand Engine - Main Dispatcher
# ============================================================================

# Run the strand engine to find and process work
# Iterates through strands 1-7 in priority order, stopping when work is found
#
# Usage: _needle_strand_engine <workspace> <agent>
# Arguments:
#   workspace - The workspace path to process
#   agent     - The agent identifier (e.g., "claude-anthropic-sonnet-alpha")
#
# Return values:
#   0 - Work was found and processed by a strand
#   1 - No work found across all enabled strands
#
# Example:
#   if _needle_strand_engine "/workspace/myproject" "claude-anthropic-sonnet-alpha"; then
#       echo "Work completed"
#   else
#       echo "No work found"
#   fi
_needle_strand_engine() {
    local workspace="$1"
    local agent="$2"

    # Strand order array (priority waterfall)
    local strands=(pluck explore mend weave unravel pulse knot)
    local strand_num=1

    _needle_debug "Starting strand engine for workspace: $workspace, agent: $agent"

    for strand in "${strands[@]}"; do
        _needle_verbose "Checking strand $strand_num: $strand"

        # Check if strand is enabled
        if ! _needle_is_strand_enabled "$strand"; then
            _needle_emit_event "strand.skipped" \
                "Strand $strand ($strand_num) is disabled" \
                "strand=$strand_num" \
                "name=$strand" \
                "reason=disabled"
            _needle_debug "Strand $strand ($strand_num) is disabled, skipping"
            ((strand_num++))
            continue
        fi

        # Emit strand started event
        _needle_emit_event "strand.started" \
            "Starting strand $strand ($strand_num)" \
            "strand=$strand_num" \
            "name=$strand" \
            "workspace=$workspace" \
            "agent=$agent"

        _needle_verbose "Dispatching to strand: $strand"

        # Dispatch to strand implementation (defined in separate files)
        # Each strand function returns:
        #   0 - Work was found and processed
        #   1 - No work found (fallthrough to next strand)
        local result
        "_needle_strand_$strand" "$workspace" "$agent"
        result=$?

        if [[ $result -eq 0 ]]; then
            # Work found and processed
            _needle_emit_event "strand.completed" \
                "Strand $strand ($strand_num) found work" \
                "strand=$strand_num" \
                "name=$strand" \
                "result=work_found"

            _needle_success "Strand $strand ($strand_num): work completed"
            return 0  # Work done, exit engine
        fi

        # Fallthrough to next strand
        _needle_emit_event "strand.fallthrough" \
            "Strand $strand ($strand_num) found no work, continuing" \
            "from=$strand_num" \
            "from_name=$strand" \
            "reason=no_work" \
            "to=$((strand_num + 1))"

        _needle_verbose "Strand $strand ($strand_num): no work found, continuing"

        ((strand_num++))
    done

    # All strands exhausted, no work found
    _needle_debug "All strands exhausted, no work found"
    return 1
}

# ============================================================================
# Utility Functions
# ============================================================================

# Get list of all strand names in order
# Usage: _needle_strand_list
# Returns: Space-separated list of strand names
_needle_strand_list() {
    echo "pluck explore mend weave unravel pulse knot"
}

# Get strand number by name
# Usage: _needle_strand_number <strand_name>
# Returns: Strand number (1-7) or 0 if not found
_needle_strand_number() {
    local strand="$1"
    case "$strand" in
        pluck)   echo 1 ;;
        explore) echo 2 ;;
        mend)    echo 3 ;;
        weave)   echo 4 ;;
        unravel) echo 5 ;;
        pulse)   echo 6 ;;
        knot)    echo 7 ;;
        *)       echo 0 ;;
    esac
}

# Get strand name by number
# Usage: _needle_strand_name <strand_number>
# Returns: Strand name or empty string if not found
_needle_strand_name() {
    local num="$1"
    case "$num" in
        1) echo "pluck"   ;;
        2) echo "explore" ;;
        3) echo "mend"    ;;
        4) echo "weave"   ;;
        5) echo "unravel" ;;
        6) echo "pulse"   ;;
        7) echo "knot"    ;;
        *) echo ""        ;;
    esac
}
