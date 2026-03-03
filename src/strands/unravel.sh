#!/usr/bin/env bash
# NEEDLE Strand: unravel (Priority 5)
# Create alternatives for blocked HUMAN beads
#
# Implementation: nd-20p
#
# This strand detects blocked HUMAN beads that have been waiting for input
# and proposes alternative approaches that can make progress without the
# human decision. This helps prevent work from stalling when waiting on
# human input that may not come quickly.
#
# Usage:
#   _needle_strand_unravel <workspace> <agent>
#
# Return values:
#   0 - Work was found and processed (alternatives created)
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

# Source agent dispatcher
if [[ -z "${_NEEDLE_DISPATCH_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../agent/dispatch.sh"
fi

# ============================================================================
# Configuration Defaults
# ============================================================================

# Default settings (can be overridden via config.yaml)
NEEDLE_UNRAVEL_MIN_WAIT_HOURS="${NEEDLE_UNRAVEL_MIN_WAIT_HOURS:-24}"
NEEDLE_UNRAVEL_MAX_ALTERNATIVES="${NEEDLE_UNRAVEL_MAX_ALTERNATIVES:-3}"
NEEDLE_UNRAVEL_TIMEOUT="${NEEDLE_UNRAVEL_TIMEOUT:-120}"

# ============================================================================
# Main Strand Entry Point
# ============================================================================

# Main unravel strand function
# Searches for blocked HUMAN beads and creates alternative approaches
#
# Usage: _needle_strand_unravel <workspace> <agent>
# Arguments:
#   workspace - The workspace path to process
#   agent     - The agent identifier (e.g., "claude-anthropic-sonnet")
#
# Return values:
#   0 - Work was found and processed (alternatives created)
#   1 - No work found (fallthrough to next strand)
_needle_strand_unravel() {
    local workspace="$1"
    local agent="$2"

    _needle_debug "unravel strand: checking for blocked HUMAN beads needing alternatives"

    # Validate inputs
    if [[ -z "$workspace" ]]; then
        _needle_error "unravel strand: workspace is required"
        return 1
    fi

    if [[ -z "$agent" ]]; then
        _needle_error "unravel strand: agent is required"
        return 1
    fi

    # Check if workspace exists
    if [[ ! -d "$workspace" ]]; then
        _needle_debug "unravel: workspace does not exist: $workspace"
        return 1
    fi

    # Check if unravel is enabled (opt-in only - default is false)
    if ! _needle_unravel_is_enabled; then
        _needle_debug "unravel: strand is disabled (opt-in)"
        return 1
    fi

    # Get configuration
    local min_wait_hours
    min_wait_hours=$(_needle_unravel_get_min_wait_hours)

    local min_wait_seconds=$((min_wait_hours * 3600))
    local now
    now=$(date +%s)

    # Find HUMAN-type beads that are blocked (waiting for input)
    local human_beads
    human_beads=$(br list --workspace="$workspace" --status blocked --type human --json 2>/dev/null)

    if [[ -z "$human_beads" ]] || [[ "$human_beads" == "[]" ]] || [[ "$human_beads" == "null" ]]; then
        _needle_debug "unravel: no blocked HUMAN beads found"
        return 1
    fi

    _needle_verbose "unravel: found blocked HUMAN beads to analyze"

    # Process each HUMAN bead
    while IFS= read -r bead; do
        [[ -z "$bead" ]] && continue

        # Extract bead details
        local bead_id created created_epoch age
        bead_id=$(echo "$bead" | jq -r '.id // empty' 2>/dev/null)
        created=$(echo "$bead" | jq -r '.created_at // .created // empty' 2>/dev/null)

        if [[ -z "$bead_id" ]]; then
            _needle_debug "unravel: skipping bead with no ID"
            continue
        fi

        # Calculate bead age
        if [[ -n "$created" ]]; then
            # Try to parse the timestamp
            if [[ "$created" =~ ^[0-9]+$ ]]; then
                # Already a unix timestamp
                created_epoch="$created"
            else
                # Try to parse as ISO date
                created_epoch=$(date -d "$created" +%s 2>/dev/null || echo "0")
            fi
        else
            created_epoch=0
        fi

        # Skip if we couldn't parse the timestamp
        if [[ "$created_epoch" == "0" ]]; then
            _needle_debug "unravel: skipping bead $bead_id - couldn't parse created timestamp"
            continue
        fi

        age=$((now - created_epoch))

        # Skip if not waiting long enough
        if ((age < min_wait_seconds)); then
            local hours_waited=$((age / 3600))
            _needle_verbose "unravel: bead $bead_id only waited ${hours_waited}h (need ${min_wait_hours}h)"
            continue
        fi

        # Check if we already created alternatives for this bead
        local existing_count
        existing_count=$(_needle_unravel_count_alternatives "$workspace" "$bead_id")

        if ((existing_count > 0)); then
            _needle_debug "unravel: bead $bead_id already has $existing_count alternative(s)"
            continue
        fi

        _needle_info "unravel: analyzing bead $bead_id (waited $((age / 3600))h) for alternatives"

        # Build unravel analysis prompt
        local prompt
        prompt=$(_needle_unravel_build_prompt "$bead_id" "$workspace" "$bead")

        # Run analysis using agent dispatcher
        local result
        result=$(_needle_dispatch_agent "$agent" "$workspace" "$prompt" "$bead_id" "unravel-analysis" "$(_needle_unravel_get_timeout)")

        local dispatch_exit=$?
        local exit_code duration output_file

        if [[ $dispatch_exit -ne 0 ]] || [[ -z "$result" ]]; then
            _needle_warn "unravel: analysis dispatch failed for bead $bead_id"
            continue
        fi

        # Parse dispatch result
        IFS='|' read -r exit_code duration output_file <<< "$result"

        if [[ "$exit_code" -ne 0 ]]; then
            _needle_warn "unravel: analysis failed with exit code $exit_code for bead $bead_id"
            [[ -f "$output_file" ]] && rm -f "$output_file"
            continue
        fi

        # Read analysis output
        local analysis
        if [[ -f "$output_file" ]]; then
            analysis=$(cat "$output_file")
            rm -f "$output_file"
        else
            _needle_warn "unravel: no output file from analysis"
            continue
        fi

        # Parse alternatives from analysis
        local alternatives
        alternatives=$(_needle_unravel_parse_alternatives "$analysis")

        if [[ -z "$alternatives" ]] || [[ "$alternatives" == "[]" ]]; then
            _needle_debug "unravel: no alternatives found for bead $bead_id"
            continue
        fi

        # Create alternative beads
        local created_count
        created_count=$(_needle_unravel_create_alternatives "$workspace" "$bead_id" "$alternatives")

        if [[ "$created_count" -gt 0 ]]; then
            _needle_success "unravel: created $created_count alternative(s) for bead $bead_id"

            # Emit completion event
            _needle_emit_event "unravel.alternatives_created" \
                "Created alternative approaches for blocked HUMAN bead" \
                "parent_id=$bead_id" \
                "alternatives_count=$created_count" \
                "workspace=$workspace" \
                "waited_hours=$((age / 3600))"

            # Return success - we found work
            return 0
        fi

    done < <(echo "$human_beads" | jq -c '.[]' 2>/dev/null)

    # No alternatives created
    _needle_debug "unravel: no alternatives created in this cycle"
    return 1
}

# ============================================================================
# Configuration Accessors
# ============================================================================

# Check if unravel strand is enabled
# Returns: 0 if enabled, 1 if disabled
_needle_unravel_is_enabled() {
    local enabled
    enabled=$(get_config "strands.unravel" "false" 2>/dev/null)

    case "$enabled" in
        true|True|TRUE|yes|Yes|YES|1)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

# Get minimum wait hours before considering alternatives
# Returns: Number of hours (default: 24)
_needle_unravel_get_min_wait_hours() {
    get_config "unravel.min_wait_hours" "$NEEDLE_UNRAVEL_MIN_WAIT_HOURS" 2>/dev/null
}

# Get maximum alternatives to create per HUMAN bead
# Returns: Maximum count (default: 3)
_needle_unravel_get_max_alternatives() {
    get_config "unravel.max_alternatives" "$NEEDLE_UNRAVEL_MAX_ALTERNATIVES" 2>/dev/null
}

# Get timeout for analysis in seconds
# Returns: Timeout seconds (default: 120)
_needle_unravel_get_timeout() {
    get_config "unravel.timeout" "$NEEDLE_UNRAVEL_TIMEOUT" 2>/dev/null
}

# ============================================================================
# Alternative Detection
# ============================================================================

# Count existing alternatives for a HUMAN bead
# Returns: Number of existing alternative beads
_needle_unravel_count_alternatives() {
    local workspace="$1"
    local parent_id="$2"

    local count
    count=$(br list --workspace="$workspace" --label "for-$parent_id" --json 2>/dev/null | jq 'length' 2>/dev/null || echo "0")

    echo "$count"
}

# ============================================================================
# Prompt Building
# ============================================================================

# Build the unravel analysis prompt
# Returns: Prompt string for agent analysis
_needle_unravel_build_prompt() {
    local bead_id="$1"
    local workspace="$2"
    local bead_json="$3"

    # Extract bead details
    local title description
    title=$(echo "$bead_json" | jq -r '.title // "Untitled"')
    description=$(echo "$bead_json" | jq -r '.description // .body // ""')

    local max_alternatives
    max_alternatives=$(_needle_unravel_get_max_alternatives)

    cat << PROMPT_EOF
You are analyzing a blocked HUMAN bead to propose alternative approaches that can make progress without requiring the human decision.

## HUMAN Bead Details
- **ID:** $bead_id
- **Title:** $title
- **Description:**
$description

## Workspace
$workspace

## Task
Propose reversible alternative approaches that could make progress on the underlying goal WITHOUT requiring the human input that is blocking this bead.

## Alternative Criteria
Good alternatives should:
1. **Be reversible** - Can be undone if the human later provides different direction
2. **Make progress** - Advance the underlying goal in some way
3. **Not conflict** - Should not block or contradict potential human decisions
4. **Be independent** - Can be pursued in parallel with the original blocked approach

## Constraints
- Maximum $max_alternatives alternatives to propose
- Each alternative should be a distinct, actionable approach
- If no good alternatives exist, return empty array
- Prioritize low-risk, high-value alternatives

## Output Format
Return a JSON object with your analysis:

\`\`\`json
{
  "alternatives": [
    {
      "title": "Short descriptive title for this alternative",
      "description": "Detailed description of the alternative approach",
      "approach": "Step-by-step implementation plan",
      "reversible": true,
      "risks": ["list of potential risks"],
      "benefits": ["list of expected benefits"]
    }
  ],
  "reasoning": "Brief explanation of why these alternatives were chosen",
  "recommendation": "Which alternative is recommended (if any)"
}
\`\`\`

Analyze the HUMAN bead and propose alternatives.
PROMPT_EOF
}

# ============================================================================
# Alternative Parsing
# ============================================================================

# Parse alternatives from agent analysis output
# Returns: JSON array of alternative objects
_needle_unravel_parse_alternatives() {
    local analysis="$1"

    # Try to extract JSON from the analysis
    local json_content

    # Look for JSON code block
    if [[ "$analysis" =~ \`\`\`json[[:space:]]*(\{[\s\S]*\})[[:space:]]*\`\`\` ]]; then
        json_content="${BASH_REMATCH[1]}"
    elif [[ "$analysis" =~ \`\`\`[[:space:]]*(\{[\s\S]*\})[[:space:]]*\`\`\` ]]; then
        json_content="${BASH_REMATCH[1]}"
    else
        # Try to find raw JSON object
        json_content=$(echo "$analysis" | grep -oP '\{[\s\S]*"alternatives"[\s\S]*\}' | head -1)
    fi

    if [[ -z "$json_content" ]]; then
        _needle_debug "unravel: no JSON found in analysis output"
        echo "[]"
        return 0
    fi

    # Extract alternatives array
    if _needle_command_exists jq; then
        local alternatives
        alternatives=$(echo "$json_content" | jq -c '.alternatives // []' 2>/dev/null)

        if [[ -z "$alternatives" ]] || [[ "$alternatives" == "null" ]]; then
            echo "[]"
            return 0
        fi

        echo "$alternatives"
    else
        # Fallback without jq - return empty
        _needle_warn "unravel: jq required for alternative parsing"
        echo "[]"
    fi
}

# ============================================================================
# Alternative Bead Creation
# ============================================================================

# Create alternative beads from parsed alternatives
# Returns: Number of beads created
_needle_unravel_create_alternatives() {
    local workspace="$1"
    local parent_id="$2"
    local alternatives="$3"

    local max_alternatives
    max_alternatives=$(_needle_unravel_get_max_alternatives)

    local created=0

    # Process each alternative
    while IFS= read -r alt && ((created < max_alternatives)); do
        [[ -z "$alt" ]] && continue

        # Extract alternative fields
        local title description approach reversible risks benefits

        if _needle_command_exists jq; then
            title=$(echo "$alt" | jq -r '.title // empty' 2>/dev/null)
            description=$(echo "$alt" | jq -r '.description // empty' 2>/dev/null)
            approach=$(echo "$alt" | jq -r '.approach // empty' 2>/dev/null)
            reversible=$(echo "$alt" | jq -r '.reversible // true' 2>/dev/null)
            risks=$(echo "$alt" | jq -r '.risks // [] | join(", ")' 2>/dev/null)
            benefits=$(echo "$alt" | jq -r '.benefits // [] | join(", ")' 2>/dev/null)
        else
            continue
        fi

        # Skip if no title
        if [[ -z "$title" ]]; then
            _needle_debug "unravel: skipping alternative with no title"
            continue
        fi

        # Build full description with approach details
        local full_description="$description"

        if [[ -n "$approach" ]]; then
            full_description+="\n\n## Approach\n$approach"
        fi

        if [[ -n "$risks" ]]; then
            full_description+="\n\n## Risks\n- $risks"
        fi

        if [[ -n "$benefits" ]]; then
            full_description+="\n\n## Benefits\n- $benefits"
        fi

        if [[ "$reversible" == "true" ]]; then
            full_description+="\n\n**Reversible:** Yes - can be undone if human provides different direction"
        else
            full_description+="\n\n**Reversible:** No - proceed with caution"
        fi

        full_description+="\n\n---\n**Alternative to:** $parent_id"

        # Create the alternative bead
        local bead_id
        bead_id=$(br create \
            --title "$title" \
            --description "$full_description" \
            --priority 2 \
            --type task \
            --label "alternative" \
            --label "for-$parent_id" \
            --label "pending-human-review" \
            --silent 2>/dev/null)

        if [[ $? -eq 0 ]] && [[ -n "$bead_id" ]]; then
            _needle_info "unravel: created alternative bead: $bead_id - $title"

            # Emit event for each alternative created
            _needle_emit_event "unravel.alternative_created" \
                "Created alternative bead" \
                "parent_id=$parent_id" \
                "alternative_id=$bead_id" \
                "title=$title" \
                "reversible=$reversible" \
                "workspace=$workspace"

            ((created++))
        else
            _needle_warn "unravel: failed to create alternative bead: $title"
        fi
    done < <(echo "$alternatives" | jq -c '.[]' 2>/dev/null)

    echo "$created"
}

# ============================================================================
# Utility Functions
# ============================================================================

# Check if a command exists (fallback if utils.sh not loaded)
if ! declare -f _needle_command_exists &>/dev/null; then
    _needle_command_exists() {
        command -v "$1" &>/dev/null
    }
fi

# Get statistics about unravel strand activity
# Usage: _needle_unravel_stats
# Returns: JSON object with stats
_needle_unravel_stats() {
    local enabled="false"
    if _needle_unravel_is_enabled; then
        enabled="true"
    fi

    local min_wait
    min_wait=$(_needle_unravel_get_min_wait_hours)

    local max_alts
    max_alts=$(_needle_unravel_get_max_alternatives)

    _needle_json_object \
        "enabled=$enabled" \
        "min_wait_hours=$min_wait" \
        "max_alternatives=$max_alts" \
        "strand=unravel" \
        "priority=5"
}

# Manually trigger unravel analysis for a specific HUMAN bead (for testing)
# Usage: _needle_unravel_run <workspace> <agent> <human_bead_id>
_needle_unravel_run() {
    local workspace="$1"
    local agent="$2"
    local human_bead_id="$3"

    if [[ -z "$human_bead_id" ]]; then
        _needle_error "Usage: _needle_unravel_run <workspace> <agent> <human_bead_id>"
        return 1
    fi

    # Get bead JSON
    local bead_json
    bead_json=$(br show "$human_bead_id" --json 2>/dev/null)

    if [[ -z "$bead_json" ]] || [[ "$bead_json" == "null" ]]; then
        _needle_error "Could not find bead: $human_bead_id"
        return 1
    fi

    # Build prompt
    local prompt
    prompt=$(_needle_unravel_build_prompt "$human_bead_id" "$workspace" "$bead_json")

    # Run analysis
    local result
    result=$(_needle_dispatch_agent "$agent" "$workspace" "$prompt" "$human_bead_id" "unravel-manual" 120)

    local exit_code duration output_file
    IFS='|' read -r exit_code duration output_file <<< "$result"

    if [[ "$exit_code" -ne 0 ]]; then
        _needle_error "Analysis failed with exit code $exit_code"
        [[ -f "$output_file" ]] && rm -f "$output_file"
        return 1
    fi

    # Read and display analysis
    if [[ -f "$output_file" ]]; then
        cat "$output_file"
        rm -f "$output_file"
    fi
}

# ============================================================================
# Direct Execution Support (for testing)
# ============================================================================

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-}" in
        run)
            if [[ $# -lt 3 ]]; then
                echo "Usage: $0 run <workspace> <agent>"
                exit 1
            fi
            _needle_strand_unravel "$2" "$3"
            exit $?
            ;;
        stats)
            _needle_unravel_stats | jq .
            ;;
        analyze)
            if [[ $# -lt 4 ]]; then
                echo "Usage: $0 analyze <workspace> <agent> <human_bead_id>"
                exit 1
            fi
            _needle_unravel_run "$2" "$3" "$4"
            ;;
        -h|--help)
            echo "Usage: $0 <command> [args]"
            echo ""
            echo "Commands:"
            echo "  run <workspace> <agent>             Run the unravel strand"
            echo "  stats                               Show strand statistics"
            echo "  analyze <ws> <agent> <human_bead>   Analyze a specific HUMAN bead"
            echo ""
            echo "The unravel strand:"
            echo "  1. Finds blocked HUMAN beads waiting for input"
            echo "  2. Analyzes beads that have waited > min_wait_hours"
            echo "  3. Proposes alternative approaches using AI analysis"
            echo "  4. Creates alternative beads labeled for human review"
            echo ""
            echo "Configuration (config.yaml):"
            echo "  strands.unravel: false        # Opt-in only (default: disabled)"
            echo "  unravel.min_wait_hours: 24    # Hours before considering alternatives"
            echo "  unravel.max_alternatives: 3   # Max alternatives per HUMAN bead"
            ;;
        *)
            echo "Unknown command: ${1:-}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
fi
