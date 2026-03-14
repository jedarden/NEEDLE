#!/usr/bin/env bash
# NEEDLE Strand Engine - Configurable strand dispatcher
#
# The strand engine iterates through a configured list of strand scripts
# in order. Each strand follows a simple contract:
#   - Function: _needle_strand_<name>(workspace, agent)
#   - Returns 0 if work was found (engine exits, worker loop restarts)
#   - Returns 1 if no work found (engine continues to next strand)
#
# Strand list is configured in ~/.needle/config.yaml as an ordered YAML list.
# Position in the list determines priority. Presence in the list means enabled.
#
# Usage:
#   source "$NEEDLE_SRC/strands/engine.sh"
#   _needle_strand_engine "$workspace" "$agent"
#
# Return values:
#   0 - Work was found and processed
#   1 - No work found across all strands

# ============================================================================
# PATH Setup (CRITICAL: Must be done before any br calls)
# ============================================================================
# Ensure ~/.local/bin and system paths are in PATH
# ~/.local/bin: br CLI access (fixes worker starvation)
# /usr/bin, /usr/local/bin: sqlite3 and other system tools (fixes mend release)
for _needle_path_dir in "$HOME/.local/bin" "/usr/local/bin" "/usr/bin"; do
    if [[ -d "$_needle_path_dir" ]]; then
        case ":$PATH:" in
            *":$_needle_path_dir:"*) ;;
            *) export PATH="$_needle_path_dir:$PATH" ;;
        esac
    fi
done
unset _needle_path_dir

# Get NEEDLE_SRC if not already set
if [[ -z "${NEEDLE_SRC:-}" ]]; then
    NEEDLE_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi

# Source diagnostic module first for logging
if [[ -z "${_NEEDLE_DIAGNOSTIC_LOADED:-}" ]]; then
    source "$NEEDLE_SRC/lib/diagnostic.sh"
fi

# ============================================================================
# Strand Loading
# ============================================================================

# Resolve a strand path entry to an absolute path.
# Relative paths resolve against NEEDLE_SRC.
# Returns: absolute path on stdout, or empty string if not found
_needle_resolve_strand_path() {
    local entry="$1"

    # Absolute path — use as-is
    if [[ "$entry" == /* ]]; then
        echo "$entry"
        return
    fi

    # Relative path — resolve against NEEDLE_SRC
    echo "$NEEDLE_SRC/$entry"
}

# Derive the strand function name from a script path.
# strands/pluck.sh → _needle_strand_pluck
# /home/user/custom_strand.sh → _needle_strand_custom_strand
_needle_strand_func_name() {
    local path="$1"
    local base
    base="$(basename "$path" .sh)"
    echo "_needle_strand_${base}"
}

# Read the strand list from config.
# Returns one strand path per line on stdout.
_needle_get_strand_list() {
    local config
    config=$(load_config 2>/dev/null)

    if [[ -z "$config" ]]; then
        return 1
    fi

    # Extract strands array entries
    if command -v jq &>/dev/null; then
        echo "$config" | jq -r '.strands[]? // empty' 2>/dev/null
    elif command -v yq &>/dev/null; then
        echo "$config" | yq '.strands[]' 2>/dev/null
    fi
}

# Source a strand script if its function is not already defined.
# This handles both dev mode (source from file) and bundled builds
# (function already exists from the single-file bundle).
_needle_load_strand() {
    local path="$1"
    local func_name="$2"

    # Already loaded (bundled build or previously sourced)
    if declare -f "$func_name" &>/dev/null; then
        return 0
    fi

    # Source the script
    if [[ -f "$path" ]]; then
        source "$path"
    else
        _needle_warn "Strand script not found: $path"
        return 1
    fi

    # Verify the function is now defined
    if ! declare -f "$func_name" &>/dev/null; then
        _needle_warn "Strand script $path does not define $func_name"
        return 1
    fi

    return 0
}

# ============================================================================
# Strand Enablement
# ============================================================================

# Check if a strand is enabled based on config (true/false/auto).
# Config format: strands.<name>: true|false|auto
# "auto" enables essential strands (pluck, explore, mend, knot) and
# disables optional ones (weave, unravel, pulse) unless the billing
# module is loaded.
#
# Usage: _needle_is_strand_enabled <strand_name>
# Returns: 0 if enabled, 1 if disabled
_needle_is_strand_enabled() {
    local strand="$1"

    # Use billing model module if available (respects auto, true, false settings)
    if declare -f _needle_billing_is_strand_enabled &>/dev/null; then
        _needle_billing_is_strand_enabled "$strand"
        return $?
    fi

    # Fallback: use config directly
    local enabled
    enabled="$(get_config "strands.$strand" "true" 2>/dev/null)"

    case "$enabled" in
        true|True|TRUE|yes|Yes|YES|1)
            return 0
            ;;
        false|False|FALSE|no|No|NO|0)
            return 1
            ;;
        auto|Auto|AUTO)
            # Without billing module, enable essential strands only
            case "$strand" in
                pluck|explore|mend|knot)
                    return 0
                    ;;
                *)
                    # Check config — if explicitly set to auto, enable all
                    # (user opted in by setting auto rather than false)
                    return 0
                    ;;
            esac
            ;;
        *)
            # Unknown value — default to enabled
            return 0
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

    # Hardcoded strand order (priority waterfall).
    # 1. pluck    - Claim open beads
    # 2. mend     - Reclaim stale/hung in_progress beads (confirm worker liveness)
    # 3. explore  - Search child folders for .beads/, then walk upward to siblings
    #              Returns 2 + NEEDLE_EXPLORE_NEW_WORKSPACE to change workspace
    #              and restart the loop (so pluck can claim in the new workspace)
    # 4. weave    - Gap analysis: docs/code vs intended goals, create beads
    # 5. pulse    - Security scan / utility checks, create beads as needed
    # 6. unravel  - Attempt to solve human-tagged beads without human
    # 7. knot     - Alert the human (failure state)
    # Enablement is controlled by config (true/false/auto per strand name).
    local strands=(pluck mend explore weave pulse unravel knot)

    # DIAGNOSTIC: Log strand engine invocation with full context
    _needle_diag_engine "Strand engine started" \
        "workspace=$workspace" \
        "agent=$agent" \
        "session=${NEEDLE_SESSION:-unknown}" \
        "verbose=${NEEDLE_VERBOSE:-false}" \
        "path=$PATH" \
        "br_available=$(command -v br &>/dev/null && echo 'yes' || echo 'no')"

    _needle_debug "Starting strand engine for workspace: $workspace, agent: $agent"

    # Track strand results for final diagnostic
    local strand_results=()
    local disabled_count=0
    local enabled_count=0
    local strand_count=${#strands[@]}

    # Use index-based loop so explore can restart it via workspace change
    local strand_idx=0
    local max_restarts=5  # Prevent infinite loops
    local restart_count=0

    while (( strand_idx < strand_count )); do
        local strand="${strands[$strand_idx]}"
        local strand_num=$((strand_idx + 1))

        _needle_verbose "Checking strand $strand_num/$strand_count: $strand"

        # Check if strand is enabled via config (true/false/auto)
        if ! _needle_is_strand_enabled "$strand"; then
            _needle_diag_engine "Strand disabled, skipping" \
                "strand=$strand" \
                "strand_num=$strand_num"

            _needle_debug "Strand $strand ($strand_num) is disabled, skipping"
            strand_results+=("$strand:disabled")
            ((disabled_count++))
            ((strand_idx++))
            continue
        fi

        ((enabled_count++))

        local func_name="_needle_strand_${strand}"

        # Verify strand function exists
        if ! declare -f "$func_name" &>/dev/null; then
            strand_results+=("$strand:not_defined")
            ((strand_idx++))
            continue
        fi

        _needle_diag_engine "Dispatching to strand" \
            "strand=$strand" \
            "strand_num=$strand_num" \
            "workspace=$workspace"

        _needle_verbose "Dispatching to strand: $strand"

        # Dispatch to strand
        local result
        "$func_name" "$workspace" "$agent"
        result=$?

        _needle_diag_engine "Strand returned result" \
            "strand=$strand" \
            "strand_num=$strand_num" \
            "result=$result"

        if [[ $result -eq 2 ]] && [[ -n "${NEEDLE_EXPLORE_NEW_WORKSPACE:-}" ]]; then
            # Explore found a workspace with beads — change workspace and restart
            workspace="$NEEDLE_EXPLORE_NEW_WORKSPACE"
            unset NEEDLE_EXPLORE_NEW_WORKSPACE

            ((restart_count++))
            if (( restart_count > max_restarts )); then
                _needle_warn "Engine hit max restarts ($max_restarts), stopping"
                break
            fi

            _needle_telemetry_emit "engine.workspace_changed" "info" \
                "new_workspace=$workspace" \
                "changed_by=$strand" \
                "restart_count=$restart_count"

            _needle_info "Explore changed workspace to $workspace, restarting strands"

            # Reset and restart from pluck
            strand_results=()
            disabled_count=0
            enabled_count=0
            strand_idx=0
            continue
        fi

        if [[ $result -eq 0 ]]; then
            # Work found and processed
            _needle_diag_engine "Work found and completed" \
                "strand=$strand" \
                "strand_num=$strand_num" \
                "strands_checked=$strand_num" \
                "total_strands=$strand_count"

            _needle_success "Strand $strand ($strand_num): work completed"
            return 0  # Work done, exit engine
        fi

        # Fallthrough to next strand
        _needle_verbose "Strand $strand ($strand_num): no work found, continuing"
        strand_results+=("$strand:no_work")
        ((strand_idx++))
    done

    # All strands exhausted, no work found
    _needle_diag_no_work "$strand_count" \
        "workspace=$workspace" \
        "agent=$agent" \
        "total_strands=$strand_count" \
        "results=${strand_results[*]}"

    _needle_diag_starvation "all_strands_exhausted" \
        "workspace=$workspace" \
        "total_strands=$strand_count" \
        "check_database=true"

    _needle_debug "All strands exhausted, no work found"
    return 1
}

# ============================================================================
# Utility Functions
# ============================================================================

# Get list of configured strand names in order
# Usage: _needle_strand_list
# Returns: Space-separated list of strand names (derived from config paths)
_needle_strand_list() {
    local names=()
    while IFS= read -r entry; do
        [[ -z "$entry" ]] && continue
        names+=("$(basename "$entry" .sh)")
    done < <(_needle_get_strand_list)
    echo "${names[*]}"
}
