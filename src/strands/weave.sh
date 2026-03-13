#!/usr/bin/env bash
# NEEDLE Strand: weave (Priority 4)
# Create beads from documentation gaps
#
# Implementation: nd-27u
#
# This strand analyzes documentation (ADRs, TODOs, ROADMAPs, READMEs) and
# identifies features or tasks mentioned in docs that are not yet tracked
# as beads. It automatically creates beads for documentation gaps.
#
# Usage:
#   _needle_strand_weave <workspace> <agent>
#
# Return values:
#   0 - Work was found and processed (beads created)
#   1 - No work found (fallthrough to next strand)

# Source bead claim module for _needle_create_bead
if [[ -z "${_NEEDLE_CLAIM_LOADED:-}" ]]; then
    NEEDLE_SRC="${NEEDLE_SRC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
    source "$NEEDLE_SRC/bead/claim.sh"
fi

# ============================================================================
# Main Strand Entry Point
# ============================================================================

_needle_strand_weave() {
    local workspace="$1"
    local agent="$2"

    _needle_debug "weave strand: scanning for documentation gaps in $workspace"

    # Check if workspace exists
    if [[ ! -d "$workspace" ]]; then
        _needle_debug "weave: workspace does not exist: $workspace"
        return 1
    fi

    # Check if weave is enabled (opt-in only - default is false)
    if ! _needle_weave_is_enabled; then
        _needle_debug "weave: strand is disabled (opt-in, set strands.weave: true to enable)"
        return 1
    fi

    # Check frequency limit (don't run every loop)
    if ! _needle_weave_check_frequency "$workspace"; then
        _needle_debug "weave: frequency limit not reached, skipping"
        return 1
    fi

    # Find documentation files
    local doc_files
    doc_files=$(_needle_weave_find_docs "$workspace")

    if [[ -z "$doc_files" ]]; then
        _needle_debug "weave: no documentation files found in $workspace"
        return 1
    fi

    local doc_count
    doc_count=$(echo "$doc_files" | wc -l)
    _needle_verbose "weave: found $doc_count documentation file(s)"

    # Get current open beads to avoid duplicates
    local open_beads
    open_beads=$(_needle_weave_get_open_beads "$workspace")

    # Build weave prompt for analysis
    local prompt
    prompt=$(_needle_weave_build_prompt "$workspace" "$doc_files" "$open_beads")

    # Run analysis using agent dispatcher
    local result
    result=$(_needle_dispatch_agent "$agent" "$workspace" "$prompt" "weave-analysis" "Weave documentation gap analysis" 120)

    # Parse result (last line only — prior lines are agent stdout via tee)
    local last_line
    last_line=$(tail -n 1 <<< "$result")
    IFS='|' read -r exit_code duration output_file <<< "$last_line"

    if [[ "$exit_code" -ne 0 ]]; then
        _needle_warn "weave: analysis failed with exit code $exit_code"
        [[ -f "$output_file" ]] && rm -f "$output_file"
        return 1
    fi

    # Read analysis output
    local analysis
    if [[ -f "$output_file" ]]; then
        analysis=$(cat "$output_file")
        rm -f "$output_file"
    else
        _needle_warn "weave: no output file from analysis"
        return 1
    fi

    # Parse gaps from analysis
    local gaps
    gaps=$(_needle_weave_parse_gaps "$analysis")

    if [[ -z "$gaps" ]] || [[ "$gaps" == "[]" ]]; then
        _needle_debug "weave: no documentation gaps found"

        # Update last run time even when no gaps found
        _needle_weave_record_run "$workspace"

        return 1
    fi

    # Create beads from gaps
    local created
    created=$(_needle_weave_create_beads "$workspace" "$gaps")

    # Update last run time
    _needle_weave_record_run "$workspace"

    if [[ "$created" -gt 0 ]]; then
        _needle_success "weave: created $created bead(s) from documentation gaps"

        # Emit completion event
        _needle_emit_event "strand.weave.completed" \
            "Weave strand completed" \
            "beads_created=$created" \
            "workspace=$workspace" \
            "docs_analyzed=$doc_count"

        return 0
    fi

    _needle_debug "weave: no beads created"
    return 1
}

# ============================================================================
# Frequency Limiting
# ============================================================================

# Check if enough time has passed since the last weave run
# Returns: 0 if we can proceed, 1 if rate limited
_needle_weave_check_frequency() {
    local workspace="$1"

    # Get frequency from config (default: 1 hour = 3600 seconds)
    local frequency
    frequency=$(get_config "strands.weave.frequency" "3600")

    # Create workspace-specific state file
    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR"
    local last_run_file="$state_dir/weave_last_run_${workspace_hash}"

    # Ensure state directory exists
    mkdir -p "$state_dir"

    # Check if last run file exists
    if [[ -f "$last_run_file" ]]; then
        local last_ts
        last_ts=$(cat "$last_run_file" 2>/dev/null)

        # Validate timestamp
        if [[ -n "$last_ts" ]] && [[ "$last_ts" =~ ^[0-9]+$ ]]; then
            local now
            now=$(date +%s)
            local elapsed=$((now - last_ts))

            if ((elapsed < frequency)); then
                _needle_verbose "weave: rate limited (${elapsed}s since last run, need ${frequency}s)"
                return 1
            fi
        fi
    fi

    return 0
}

# Record that weave ran for this workspace
_needle_weave_record_run() {
    local workspace="$1"

    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR"
    local last_run_file="$state_dir/weave_last_run_${workspace_hash}"

    mkdir -p "$state_dir"
    date +%s > "$last_run_file"
}

# ============================================================================
# Documentation Discovery
# ============================================================================

# Find documentation files in the workspace
# Returns: List of documentation file paths (one per line)
_needle_weave_find_docs() {
    local workspace="$1"
    local max_files
    max_files=$(get_config "strands.weave.max_doc_files" "50")

    local doc_patterns=(
        "*.md"
        "ADR*.md"
        "TODO*"
        "ROADMAP*"
        "CHANGELOG*"
        "docs/**/*.md"
        "doc/**/*.md"
        "documentation/**/*.md"
    )

    local found_files=()
    local count=0

    # Search for each pattern
    for pattern in "${doc_patterns[@]}"; do
        if ((count >= max_files)); then
            break
        fi

        while IFS= read -r file; do
            if [[ -f "$file" ]] && ((count < max_files)); then
                # Skip .beads directory
                if [[ "$file" != *"/.beads/"* ]]; then
                    found_files+=("$file")
                    ((count++))
                fi
            fi
        done < <(find "$workspace" -name "$pattern" -type f 2>/dev/null | head -$((max_files - count)))
    done

    # Output unique files
    printf '%s\n' "${found_files[@]}" 2>/dev/null | sort -u
}

# ============================================================================
# Open Beads Retrieval
# ============================================================================

# Get list of open beads to avoid creating duplicates
# Returns: JSON array of open bead titles and descriptions
_needle_weave_get_open_beads() {
    local workspace="$1"

    local open_beads
    open_beads=$(br list --workspace="$workspace" --status open --priority 0,1,2,3 --json 2>/dev/null)

    if [[ -z "$open_beads" ]] || [[ "$open_beads" == "[]" ]] || [[ "$open_beads" == "null" ]]; then
        echo "[]"
        return 0
    fi

    # Extract just titles for duplicate detection
    if _needle_command_exists jq; then
        echo "$open_beads" | jq -c '[.[].title // empty]' 2>/dev/null || echo "[]"
    else
        echo "[]"
    fi
}

# ============================================================================
# Prompt Building
# ============================================================================

# Build the weave analysis prompt
# Returns: Prompt string for agent analysis
_needle_weave_build_prompt() {
    local workspace="$1"
    local doc_files="$2"
    local open_beads="$3"

    local max_beads
    max_beads=$(get_config "strands.weave.max_beads_per_run" "5")

    # Build the prompt
    cat << PROMPT_EOF
You are analyzing a codebase for gaps between documentation and implementation.

## Documentation Files
$(_needle_weave_format_doc_list "$doc_files")

## Current Open Beads
$open_beads

## Instructions

1. Read the documentation files above (ADRs, TODOs, ROADMAPs, README)
2. Identify features, tasks, or fixes mentioned in docs that:
   - Are NOT already tracked as open beads
   - Are NOT already implemented in the codebase
   - Are actionable and well-defined enough to work on

3. For each gap found, output a JSON object:
{
  "gaps": [
    {
      "title": "Brief title for the bead",
      "description": "Detailed description of what needs to be done",
      "source_file": "path/to/doc/that/mentions/this",
      "source_line": "relevant quote from documentation",
      "priority": 2,
      "type": "task|bug|feature",
      "estimated_effort": "small|medium|large"
    }
  ]
}

4. Only output gaps that are:
   - Clearly defined in documentation
   - Not duplicates of existing beads
   - Actually missing from implementation

5. If no gaps found, output: {"gaps": []}

## Priority Values
- 0 = critical (blocking issues, security concerns)
- 1 = high (important features, significant improvements)
- 2 = normal (standard tasks)
- 3 = low (nice-to-have, minor improvements)

## Constraints
- Maximum $max_beads gaps to identify
- Only include actionable items that can become beads
- Skip items that are vague or purely aspirational
- Prefer concrete, well-defined tasks
PROMPT_EOF
}

# Format documentation file list for prompt
_needle_weave_format_doc_list() {
    local doc_files="$1"
    local idx=1

    while IFS= read -r file; do
        if [[ -n "$file" ]] && [[ -f "$file" ]]; then
            echo "$idx. $file"
            ((idx++))
        fi
    done <<< "$doc_files"
}

# ============================================================================
# Gap Parsing
# ============================================================================

# Parse gaps from agent analysis output
# Returns: JSON array of gap objects
_needle_weave_parse_gaps() {
    local analysis="$1"

    # Try to extract JSON from the analysis
    local json_content

    # Look for JSON code block
    if [[ "$analysis" =~ \`\`\`json[[:space:]]*(\{.*\})[[:space:]]*\`\`\` ]]; then
        json_content="${BASH_REMATCH[1]}"
    elif [[ "$analysis" =~ \`\`\`[[:space:]]*(\{.*\})[[:space:]]*\`\`\` ]]; then
        json_content="${BASH_REMATCH[1]}"
    else
        # Try to find raw JSON object
        json_content=$(echo "$analysis" | grep -oP '\{[\s\S]*"gaps"[\s\S]*\}' | head -1)
    fi

    if [[ -z "$json_content" ]]; then
        _needle_debug "weave: no JSON found in analysis output"
        echo "[]"
        return 0
    fi

    # Extract gaps array
    if _needle_command_exists jq; then
        local gaps
        gaps=$(echo "$json_content" | jq -c '.gaps // []' 2>/dev/null)

        if [[ -z "$gaps" ]] || [[ "$gaps" == "null" ]]; then
            echo "[]"
            return 0
        fi

        echo "$gaps"
    else
        # Fallback without jq - return empty
        _needle_warn "weave: jq required for gap parsing"
        echo "[]"
    fi
}

# ============================================================================
# Bead Creation
# ============================================================================

# Create beads from identified gaps
# Returns: Number of beads created
_needle_weave_create_beads() {
    local workspace="$1"
    local gaps="$2"

    local max_beads
    max_beads=$(get_config "strands.weave.max_beads_per_run" "5")

    local created=0

    # Process each gap
    while IFS= read -r gap && ((created < max_beads)); do
        [[ -z "$gap" ]] && continue

        # Extract gap fields
        local title description priority source_file source_line bead_type labels

        if _needle_command_exists jq; then
            title=$(echo "$gap" | jq -r '.title // empty' 2>/dev/null)
            description=$(echo "$gap" | jq -r '.description // empty' 2>/dev/null)
            priority=$(echo "$gap" | jq -r '.priority // 2' 2>/dev/null)
            source_file=$(echo "$gap" | jq -r '.source_file // empty' 2>/dev/null)
            source_line=$(echo "$gap" | jq -r '.source_line // empty' 2>/dev/null)
            bead_type=$(echo "$gap" | jq -r '.type // "task"' 2>/dev/null)
            labels=$(echo "$gap" | jq -r '.labels // [] | join(",")' 2>/dev/null)
        else
            continue
        fi

        # Validate bead_type (task|bug|feature)
        case "$bead_type" in
            task|bug|feature) ;;
            *) bead_type="task" ;;
        esac

        # Skip if no title
        if [[ -z "$title" ]]; then
            _needle_debug "weave: skipping gap with no title"
            continue
        fi

        # Build full description with source context
        local full_description="$description"
        if [[ -n "$source_file" ]] || [[ -n "$source_line" ]]; then
            full_description+="\n\n---\n**Source:**"
            [[ -n "$source_file" ]] && full_description+=" $source_file"
            [[ -n "$source_line" ]] && full_description+="\n> $source_line"
        fi

        # Build label arguments
        local label_args=()
        label_args+=(--label "weave-generated")
        label_args+=(--label "from-docs")
        if [[ -n "$labels" ]]; then
            IFS=',' read -ra label_arr <<< "$labels"
            for label in "${label_arr[@]}"; do
                label_args+=(--label "$label")
            done
        fi

        # Create the bead using wrapper (handles unassigned_by_default)
        local bead_id
        bead_id=$(_needle_create_bead \
            --workspace "$workspace" \
            --title "$title" \
            --description "$full_description" \
            --priority "$priority" \
            --type "$bead_type" \
            "${label_args[@]}" \
            --silent 2>/dev/null)

        if [[ $? -eq 0 ]] && [[ -n "$bead_id" ]]; then
            _needle_info "weave: created bead: $bead_id - $title"

            # Emit event
            _needle_emit_event "weave.bead_created" \
                "Created bead from documentation gap" \
                "bead_id=$bead_id" \
                "title=$title" \
                "source=$source_file" \
                "workspace=$workspace" >&2

            ((created++))
        else
            _needle_warn "weave: failed to create bead: $title"
        fi
    done < <(echo "$gaps" | jq -c '.[]' 2>/dev/null)

    echo "$created"
}

# ============================================================================
# Utility Functions
# ============================================================================

# Check if weave strand is enabled (opt-in, false by default)
# Returns: 0 if enabled, 1 if disabled
_needle_weave_is_enabled() {
    local enabled
    enabled=$(get_config "strands.weave" "false" 2>/dev/null)

    case "$enabled" in
        true|True|TRUE|yes|Yes|YES|1)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

# Get statistics about weave strand activity
# Usage: _needle_weave_stats
# Returns: JSON object with stats
_needle_weave_stats() {
    local state_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR"

    local run_count=0
    local last_run="never"

    # Count weave run tracking files
    if [[ -d "$state_dir" ]]; then
        run_count=$(find "$state_dir" -name "weave_last_run_*" -type f 2>/dev/null | wc -l)

        # Get most recent run time
        local newest_file
        newest_file=$(find "$state_dir" -name "weave_last_run_*" -type f -printf '%T@ %p\n' 2>/dev/null | sort -rn | head -1 | cut -d' ' -f2-)

        if [[ -n "$newest_file" ]] && [[ -f "$newest_file" ]]; then
            local ts
            ts=$(cat "$newest_file" 2>/dev/null)
            if [[ -n "$ts" ]] && [[ "$ts" =~ ^[0-9]+$ ]]; then
                last_run=$(date -d "@$ts" -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo "$ts")
            fi
        fi
    fi

    _needle_json_object \
        "workspace_tracking_files=$run_count" \
        "last_run=$last_run"
}

# Clear rate limit for a workspace (for testing/manual intervention)
# Usage: _needle_weave_clear_rate_limit <workspace>
_needle_weave_clear_rate_limit() {
    local workspace="$1"

    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR"
    local last_run_file="$state_dir/weave_last_run_${workspace_hash}"

    if [[ -f "$last_run_file" ]]; then
        rm -f "$last_run_file"
        _needle_info "Cleared weave rate limit for: $workspace"
    fi
}

# Manually trigger weave analysis for testing
# Usage: _needle_weave_run <workspace> [agent]
_needle_weave_run() {
    local workspace="$1"
    local agent="${2:-default}"

    # Clear rate limit to force run
    _needle_weave_clear_rate_limit "$workspace"

    # Run weave
    _needle_strand_weave "$workspace" "$agent"
}
