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

    # NOTE: enablement check removed — presence in the strand list means enabled

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
# Includes actual file contents so the LLM can analyze gaps
_needle_weave_build_prompt() {
    local workspace="$1"
    local doc_files="$2"
    local open_beads="$3"

    local max_beads
    max_beads=$(get_config "strands.weave.max_beads_per_run" "5")

    # Build the prompt with actual file contents
    cat << PROMPT_EOF
You are performing a gap analysis on a codebase. Your job is to find gaps between
what was documented/planned and what actually exists in the implementation.

## Workspace
$workspace

## Documentation Contents
$(_needle_weave_format_doc_contents "$doc_files")

## Codebase Structure
$(_needle_weave_codebase_summary "$workspace")

## Current Open Beads (already tracked — do NOT duplicate)
$open_beads

## Current In-Progress Beads (being worked on — do NOT duplicate)
$(_needle_weave_get_in_progress_beads "$workspace")

## Instructions

Analyze the documentation contents and codebase structure above. Identify:

1. **Features described in docs but not implemented** — Look for described functionality,
   architecture, or behavior that is missing from the actual codebase.

2. **TODOs, FIXMEs, HACKs in code** — Find inline markers that indicate known gaps.

3. **Incomplete implementations** — Functions that are stubbed, partially implemented,
   or have placeholder logic (e.g., "return 0" where real logic should be).

4. **Missing tests** — Code modules without corresponding test files.

5. **Configuration gaps** — Documented config options that aren't implemented,
   or code that references config keys that don't exist in defaults.

For each gap, output a JSON object:
{
  "gaps": [
    {
      "title": "Brief actionable title",
      "description": "What needs to be done and why",
      "source_file": "path/to/file/where/gap/was/found",
      "source_line": "relevant quote or line reference",
      "priority": 2,
      "type": "task|bug|feature"
    }
  ]
}

## Rules
- Maximum $max_beads gaps per analysis
- Do NOT duplicate existing open or in-progress beads
- Only include concrete, actionable items
- Skip aspirational items without clear implementation path
- If no gaps found, output: {"gaps": []}

## Priority Values
- 0 = critical (security, data loss, blocking)
- 1 = high (important features, significant bugs)
- 2 = normal (standard tasks)
- 3 = low (nice-to-have, cleanup)
PROMPT_EOF
}

# Format documentation files with their CONTENTS for the prompt
_needle_weave_format_doc_contents() {
    local doc_files="$1"
    local max_content_per_file=200  # lines per file to include
    local idx=1

    while IFS= read -r file; do
        if [[ -n "$file" ]] && [[ -f "$file" ]]; then
            echo ""
            echo "### $idx. $file"
            echo '```'
            head -n "$max_content_per_file" "$file" 2>/dev/null
            local total_lines
            total_lines=$(wc -l < "$file" 2>/dev/null || echo 0)
            if (( total_lines > max_content_per_file )); then
                echo "... ($((total_lines - max_content_per_file)) more lines truncated)"
            fi
            echo '```'
            ((idx++))
        fi
    done <<< "$doc_files"
}

# Generate a summary of the codebase structure
_needle_weave_codebase_summary() {
    local workspace="$1"
    local max_depth=3

    echo '```'
    # Show directory tree (excluding common noise)
    if command -v tree &>/dev/null; then
        tree -L "$max_depth" -I 'node_modules|.git|vendor|.cache|__pycache__|.beads|dist|build' \
            --dirsfirst "$workspace" 2>/dev/null | head -80
    else
        find "$workspace" -maxdepth "$max_depth" -type f \
            -not -path "*/node_modules/*" \
            -not -path "*/.git/*" \
            -not -path "*/vendor/*" \
            -not -path "*/.cache/*" \
            -not -path "*/__pycache__/*" \
            -not -path "*/.beads/*" \
            -not -path "*/dist/*" \
            -not -path "*/build/*" \
            2>/dev/null | sort | head -80
    fi
    echo '```'

    # Show TODOs/FIXMEs in code
    local todo_count
    todo_count=$(grep -r -c 'TODO\|FIXME\|HACK\|XXX' "$workspace" \
        --include='*.sh' --include='*.py' --include='*.js' --include='*.ts' \
        --include='*.go' --include='*.rs' --include='*.yaml' --include='*.yml' \
        2>/dev/null | awk -F: '{sum+=$2} END {print sum+0}')

    if (( todo_count > 0 )); then
        echo ""
        echo "### Inline TODOs/FIXMEs ($todo_count found)"
        echo '```'
        grep -rn 'TODO\|FIXME\|HACK\|XXX' "$workspace" \
            --include='*.sh' --include='*.py' --include='*.js' --include='*.ts' \
            --include='*.go' --include='*.rs' --include='*.yaml' --include='*.yml' \
            -not -path "*/.beads/*" \
            -not -path "*/node_modules/*" \
            -not -path "*/.git/*" \
            2>/dev/null | head -30
        echo '```'
    fi
}

# Get in-progress beads to avoid duplicating work being done
_needle_weave_get_in_progress_beads() {
    local workspace="$1"

    local in_progress
    in_progress=$(br list --workspace="$workspace" --status in_progress --json 2>/dev/null)

    if [[ -z "$in_progress" ]] || [[ "$in_progress" == "[]" ]] || [[ "$in_progress" == "null" ]]; then
        echo "[]"
        return 0
    fi

    if _needle_command_exists jq; then
        echo "$in_progress" | jq -c '[.[] | {id, title}]' 2>/dev/null || echo "[]"
    else
        echo "[]"
    fi
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

# NOTE: _needle_weave_is_enabled removed — strand enablement is now
# controlled by presence in the config strand list

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
