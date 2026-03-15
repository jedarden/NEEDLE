#!/usr/bin/env bash
# NEEDLE Strand: weave (Priority 4)
# Gap analysis: reconstruct the intended goal, compare against what's built
#
# Implementation: nd-27u
#
# Weave reconstructs the project's intended end-state by examining:
#   1. Closed beads (completed work reveals the trajectory)
#   2. Open/in-progress beads (known remaining work)
#   3. Genesis beads (high-level plans, if any)
#   4. Git history (commit messages reveal intent)
#   5. Documentation (READMEs, ADRs, plans, roadmaps)
#
# It then compares that intent against the actual codebase to find gaps:
# features described but not built, stubs, missing tests, TODOs, etc.
# Each gap becomes a new bead.
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

    _needle_debug "weave strand: gap analysis for $workspace"

    if [[ ! -d "$workspace" ]]; then
        _needle_debug "weave: workspace does not exist: $workspace"
        return 1
    fi

    # Check if weave strand is enabled (opt-in)
    if ! _needle_weave_is_enabled; then
        _needle_debug "weave: strand is disabled"
        return 1
    fi

    # Check frequency limit (don't run every loop)
    if ! _needle_weave_check_frequency "$workspace"; then
        _needle_debug "weave: frequency limit not reached, skipping"
        return 1
    fi

    # Gather all context about the project's intent and current state
    local prompt
    prompt=$(_needle_weave_build_prompt "$workspace")

    if [[ -z "$prompt" ]]; then
        _needle_warn "weave: could not build analysis prompt"
        _needle_weave_record_run "$workspace"
        return 1
    fi

    # Run analysis using agent dispatcher
    local result
    result=$(_needle_dispatch_agent "$agent" "$workspace" "$prompt" "weave-gap-analysis" "Weave gap analysis" 180)

    # Parse result (last line only — prior lines are agent stdout via tee)
    local last_line
    last_line=$(tail -n 1 <<< "$result")
    IFS='|' read -r exit_code duration output_file <<< "$last_line"

    if [[ "$exit_code" -ne 0 ]]; then
        _needle_warn "weave: analysis failed with exit code $exit_code"
        [[ -f "$output_file" ]] && rm -f "$output_file"
        _needle_weave_record_run "$workspace"
        return 1
    fi

    # Read analysis output
    local analysis
    if [[ -f "$output_file" ]]; then
        analysis=$(cat "$output_file")
        rm -f "$output_file"
    else
        _needle_warn "weave: no output file from analysis"
        _needle_weave_record_run "$workspace"
        return 1
    fi

    # Parse gaps from analysis
    local gaps
    gaps=$(_needle_weave_parse_gaps "$analysis")

    if [[ -z "$gaps" ]] || [[ "$gaps" == "[]" ]]; then
        _needle_debug "weave: no gaps found"
        _needle_weave_record_run "$workspace"
        return 1
    fi

    # Create beads from gaps
    local created
    created=$(_needle_weave_create_beads "$workspace" "$gaps")

    _needle_weave_record_run "$workspace"

    if [[ "$created" -gt 0 ]]; then
        _needle_success "weave: created $created bead(s) from gap analysis"

        _needle_telemetry_emit "weave.scan_completed" "info" \
            "workspace=$workspace" \
            "beads_created=$created"

        return 0
    fi

    _needle_debug "weave: no beads created (all gaps were duplicates or filtered)"
    return 1
}

# ============================================================================
# Prompt Building — Reconstruct Intent, Compare Against Reality
# ============================================================================

_needle_weave_build_prompt() {
    local workspace="$1"
    local docs_arg="${2:-}"    # optional: doc file path(s)
    local open_beads="${3:-}"  # optional: JSON array of open bead titles

    # If open_beads not provided, gather from workspace
    if [[ -z "$open_beads" ]]; then
        open_beads=$(_needle_weave_get_open_beads "$workspace")
    fi

    cat << 'PROMPT_HEADER'
You are analyzing a codebase for gaps between documentation and implementation.
Find features or functionality described in docs, plans, roadmaps, or commit history
that are NOT already tracked as open beads and NOT already implemented in the codebase.

PROMPT_HEADER

    # Section 1: Closed beads — the trajectory of completed work
    echo "## Completed Work (Closed Beads)"
    echo ""
    echo "These beads have been completed. They reveal the project's trajectory"
    echo "and intended direction. Use them to understand the ultimate goal."
    echo ""
    _needle_weave_closed_beads "$workspace"
    echo ""

    # Section 2: Genesis beads — high-level plans
    echo "## Genesis / Plan Beads"
    echo ""
    echo "These are high-level planning beads that describe the intended end-state."
    echo ""
    _needle_weave_genesis_beads "$workspace"
    echo ""

    # Section 3: Current open beads — for deduplication
    echo "## Current Open Beads"
    echo ""
    echo "These beads are already tracked. Do NOT create duplicates."
    echo ""
    if [[ -n "$open_beads" ]] && [[ "$open_beads" != "[]" ]]; then
        if _needle_command_exists jq; then
            echo "$open_beads" | jq -r '.[]' 2>/dev/null | while IFS= read -r title; do
                echo "- $title"
            done
        fi
    else
        _needle_weave_existing_beads "$workspace"
    fi
    echo ""

    # Section 4: Git history — commit messages reveal intent
    echo "## Recent Git History"
    echo ""
    echo "Commit messages show what has been built and the direction of development."
    echo ""
    _needle_weave_git_history "$workspace"
    echo ""

    # Section 5: Documentation — READMEs, plans, ADRs
    echo "## Documentation"
    echo ""
    _needle_weave_doc_contents "$workspace"
    echo ""

    # Section 6: Current codebase state
    echo "## Codebase Structure"
    echo ""
    _needle_weave_codebase_summary "$workspace"
    echo ""

    # Instructions and output format
    cat << 'PROMPT_INSTRUCTIONS'

## Your Task

Using ALL of the above context, reconstruct the project's intended end-state.
Then compare that intent against what actually exists in the codebase.

Identify gaps where:
1. **Planned features are not yet built** — described in genesis beads, docs, or
   implied by the trajectory of closed beads, but missing from the codebase.
2. **Incomplete implementations** — code that is stubbed out, has placeholder logic,
   or was started but not finished (TODO/FIXME/HACK markers count).
3. **Missing integration** — components that exist individually but are not wired
   together as the plan describes.
4. **Missing tests or validation** — code paths that lack test coverage.
5. **Divergence from plan** — places where what was built differs from what was
   planned, and the plan version is still the intended goal.

Do NOT include:
- Items already tracked as open or in-progress beads (listed above).
- Vague or aspirational items without a clear implementation path.
- Items that are clearly out of scope or were intentionally descoped.

## Output Format

Return ONLY a JSON object (no markdown code blocks):

{
  "gaps": [
    {
      "title": "Concise title summarising what this bead does (e.g. 'Add rate limiting to Kalshi WebSocket client') — no generic placeholders",
      "description": "What needs to be done. Reference the source of the intent.",
      "source_file": "path/to/doc.md",
      "source_line": "The specific line or passage from the source doc",
      "priority": 2,
      "type": "task|bug|feature",
      "estimated_effort": "small|medium|large",
      "verification_cmd": "optional: shell command that exits 0 when done condition is met"
    }
  ],
  "intent_summary": "One paragraph summarizing the project's inferred goal"
}

## Verification Command (verification_cmd)

For each gap bead, if the done condition can be expressed as a shell command
that exits 0 on success and non-zero on failure, include a verification_cmd field.

This enables automated verification after agent completion. The command runs
in the workspace directory after the agent exits 0.

Examples of good verification_cmd values:
  - pytest tests/test_foo.py -q 2>&1 | grep -q passed
  - grep -q 'def new_function' src/module.py
  - curl -sf http://localhost:8080/health | jq -e '.status=="ok"'
  - [[ $(wc -l < docs/api.md) -gt 50 ]]
  - command -v new_command && grep -q 'new_command' README.md

If no reliable machine-verifiable condition exists, omit the verification_cmd
field entirely. Not all gaps need verification — only when the done condition
is naturally testable via shell command.

Create as many gaps as you find — do not artificially limit. If no gaps found: {"gaps": []}

Priority: 0=critical, 1=high, 2=normal, 3=low
Type: task|bug|feature
PROMPT_INSTRUCTIONS
}

# ============================================================================
# Context Gathering Functions
# ============================================================================

# Get closed beads — the record of completed work
_needle_weave_closed_beads() {
    local workspace="$1"
    local max_closed
    max_closed=$(get_config "strands.weave.max_closed_beads" "30")

    local closed
    closed=$(cd "$workspace" 2>/dev/null && br list --status closed --json 2>/dev/null)

    if [[ -z "$closed" ]] || [[ "$closed" == "[]" ]] || [[ "$closed" == "null" ]]; then
        echo "(No closed beads found)"
        return
    fi

    if _needle_command_exists jq; then
        echo "$closed" | jq -r --argjson max "$max_closed" '
            sort_by(.updated_at // .created_at) | reverse | .[:$max] |
            .[] | "- [\(.id)] [\(.issue_type // "task")] \(.title)\(.description // "" | if length > 200 then "\n  " + (.[0:200]) + "..." else if length > 0 then "\n  " + . else "" end end)"
        ' 2>/dev/null || echo "(Could not parse closed beads)"
    else
        echo "(jq required)"
    fi
}

# Get genesis beads — high-level plans
_needle_weave_genesis_beads() {
    local workspace="$1"

    local genesis
    genesis=$(cd "$workspace" 2>/dev/null && br list --type genesis --json 2>/dev/null)

    if [[ -z "$genesis" ]] || [[ "$genesis" == "[]" ]] || [[ "$genesis" == "null" ]]; then
        # Also try searching by label
        genesis=$(cd "$workspace" 2>/dev/null && br list --label genesis --json 2>/dev/null)
    fi

    if [[ -z "$genesis" ]] || [[ "$genesis" == "[]" ]] || [[ "$genesis" == "null" ]]; then
        # Try plan beads
        genesis=$(cd "$workspace" 2>/dev/null && br search "genesis OR plan OR roadmap" --json 2>/dev/null)
    fi

    if [[ -z "$genesis" ]] || [[ "$genesis" == "[]" ]] || [[ "$genesis" == "null" ]]; then
        echo "(No genesis or plan beads found)"
        return
    fi

    if _needle_command_exists jq; then
        echo "$genesis" | jq -r '
            .[] | "- [\(.id)] [\(.status)] \(.title)\n  \(.description // "" | if length > 500 then .[0:500] + "..." else . end)"
        ' 2>/dev/null || echo "(Could not parse genesis beads)"
    else
        echo "(jq required)"
    fi
}

# Get existing open + in-progress beads (for deduplication)
_needle_weave_existing_beads() {
    local workspace="$1"

    local existing
    existing=$(cd "$workspace" 2>/dev/null && br list --json 2>/dev/null)

    if [[ -z "$existing" ]] || [[ "$existing" == "[]" ]] || [[ "$existing" == "null" ]]; then
        echo "(No existing beads)"
        return
    fi

    if _needle_command_exists jq; then
        echo "$existing" | jq -r '
            [.[] | select(.status == "open" or .status == "in_progress")] |
            .[] | "- [\(.id)] [\(.status)] [\(.issue_type // "task")] \(.title)"
        ' 2>/dev/null || echo "(Could not parse existing beads)"
    else
        echo "(jq required)"
    fi
}

# Get git history — commit messages reveal intent
_needle_weave_git_history() {
    local workspace="$1"
    local max_commits
    max_commits=$(get_config "strands.weave.max_git_commits" "40")

    if [[ ! -d "$workspace/.git" ]]; then
        echo "(Not a git repository)"
        return
    fi

    echo '```'
    git -C "$workspace" log --oneline --no-merges -n "$max_commits" 2>/dev/null || echo "(git log failed)"
    echo '```'

    # Also show recent branch names (they often describe features)
    local branches
    branches=$(git -C "$workspace" branch --sort=-committerdate --format='%(refname:short)' 2>/dev/null | head -10)
    if [[ -n "$branches" ]]; then
        echo ""
        echo "### Recent Branches"
        echo '```'
        echo "$branches"
        echo '```'
    fi
}

# Get documentation file contents
_needle_weave_doc_contents() {
    local workspace="$1"
    local max_files
    max_files=$(get_config "strands.weave.max_doc_files" "15")
    local max_lines_per_file=150

    # Find documentation files, prioritizing plans and high-level docs
    local doc_files
    doc_files=$(find "$workspace" \
        \( -name "README.md" -o -name "ROADMAP*" -o -name "TODO*" \
           -o -name "CHANGELOG*" -o -name "plan.md" -o -name "PLAN*" \
           -o -name "ARCHITECTURE*" -o -name "DESIGN*" -o -name "ADR-*" \
           -o -name "AGENTS.md" -o -name "CLAUDE.md" \) \
        -type f \
        -not -path "*/.beads/*" \
        -not -path "*/node_modules/*" \
        -not -path "*/.git/*" \
        -not -path "*/vendor/*" \
        2>/dev/null | head -n "$max_files")

    # Also check docs/ directory
    if [[ -d "$workspace/docs" ]]; then
        local docs_dir_files
        docs_dir_files=$(find "$workspace/docs" -name "*.md" -type f \
            -not -name "worker-starvation-*" \
            2>/dev/null | head -n 10)
        if [[ -n "$docs_dir_files" ]]; then
            doc_files=$(printf '%s\n%s' "$doc_files" "$docs_dir_files" | sort -u | head -n "$max_files")
        fi
    fi

    if [[ -z "$doc_files" ]]; then
        echo "(No documentation files found)"
        return
    fi

    local idx=1
    while IFS= read -r file; do
        [[ -z "$file" ]] && continue
        [[ ! -f "$file" ]] && continue

        # Show relative path
        local rel_path="${file#$workspace/}"
        echo "### $idx. $rel_path"
        echo '```'
        head -n "$max_lines_per_file" "$file" 2>/dev/null
        local total_lines
        total_lines=$(wc -l < "$file" 2>/dev/null || echo 0)
        if (( total_lines > max_lines_per_file )); then
            echo "... ($((total_lines - max_lines_per_file)) more lines truncated)"
        fi
        echo '```'
        echo ""
        ((idx++))
    done <<< "$doc_files"
}

# Generate a summary of the codebase structure
_needle_weave_codebase_summary() {
    local workspace="$1"
    local max_depth=3

    echo '```'
    if command -v tree &>/dev/null; then
        tree -L "$max_depth" -I 'node_modules|.git|vendor|.cache|__pycache__|.beads|dist|build' \
            --dirsfirst "$workspace" 2>/dev/null | head -100
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
            2>/dev/null | sort | head -100
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

# ============================================================================
# Frequency Limiting
# ============================================================================

_needle_weave_check_frequency() {
    local workspace="$1"
    local frequency
    frequency=$(get_config "strands.weave.frequency" "3600")

    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR"
    local last_run_file="$state_dir/weave_last_run_${workspace_hash}"

    mkdir -p "$state_dir"

    if [[ -f "$last_run_file" ]]; then
        local last_ts
        last_ts=$(cat "$last_run_file" 2>/dev/null)
        if [[ -n "$last_ts" ]] && [[ "$last_ts" =~ ^[0-9]+$ ]]; then
            local now elapsed
            now=$(date +%s)
            elapsed=$((now - last_ts))
            if ((elapsed < frequency)); then
                _needle_verbose "weave: rate limited (${elapsed}s since last run, need ${frequency}s)"
                return 1
            fi
        fi
    fi

    return 0
}

_needle_weave_record_run() {
    local workspace="$1"
    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR"
    mkdir -p "$state_dir"
    date +%s > "$state_dir/weave_last_run_${workspace_hash}"
}

# ============================================================================
# Gap Parsing
# ============================================================================

_needle_weave_parse_gaps() {
    local analysis="$1"

    # Try multiple extraction methods
    local json_content=""

    # Method 1: Extract from ```json code block
    if [[ "$analysis" == *'```json'* ]]; then
        local in_block=false
        local json_lines=()
        while IFS= read -r line; do
            if [[ "$line" =~ ^\`\`\`json ]]; then
                in_block=true
                continue
            fi
            if [[ "$line" =~ ^\`\`\` ]] && [[ "$in_block" == "true" ]]; then
                break
            fi
            if [[ "$in_block" == "true" ]]; then
                json_lines+=("$line")
            fi
        done <<< "$analysis"
        json_content=$(printf '%s\n' "${json_lines[@]}")
    fi

    # Method 2: Try to find raw JSON with "gaps" key
    if [[ -z "$json_content" ]] || ! echo "$json_content" | jq -e '.gaps' &>/dev/null; then
        json_content=$(echo "$analysis" | sed -n '/{/,/}$/p' | head -200)
    fi

    # Method 3: The whole output might be JSON
    if [[ -z "$json_content" ]] || ! echo "$json_content" | jq -e '.gaps' &>/dev/null; then
        json_content="$analysis"
    fi

    if [[ -z "$json_content" ]]; then
        echo "[]"
        return 0
    fi

    if _needle_command_exists jq; then
        local gaps
        gaps=$(echo "$json_content" | jq -c '.gaps // []' 2>/dev/null)
        if [[ -z "$gaps" ]] || [[ "$gaps" == "null" ]]; then
            echo "[]"
            return 0
        fi
        echo "$gaps"
    else
        echo "[]"
    fi
}

# ============================================================================
# Bead Creation
# ============================================================================

_needle_weave_create_beads() {
    local workspace="$1"
    local gaps="$2"

    local created=0
    local max_beads
    max_beads=$(get_config "strands.weave.max_beads_per_run" "0" 2>/dev/null || echo "0")

    while IFS= read -r gap; do
        [[ -z "$gap" ]] && continue

        # Respect max_beads_per_run limit
        if [[ "$max_beads" -gt 0 ]] && [[ $created -ge $max_beads ]]; then
            break
        fi

        local title description priority source source_file source_line bead_type verification_cmd

        if _needle_command_exists jq; then
            title=$(echo "$gap" | jq -r '.title // empty' 2>/dev/null)
            description=$(echo "$gap" | jq -r '.description // empty' 2>/dev/null)
            priority=$(echo "$gap" | jq -r '.priority // 2' 2>/dev/null)
            source=$(echo "$gap" | jq -r '.source // empty' 2>/dev/null)
            source_file=$(echo "$gap" | jq -r '.source_file // empty' 2>/dev/null)
            source_line=$(echo "$gap" | jq -r '.source_line // empty' 2>/dev/null)
            bead_type=$(echo "$gap" | jq -r '.type // "task"' 2>/dev/null)
            verification_cmd=$(echo "$gap" | jq -r '.verification_cmd // empty' 2>/dev/null)
        else
            continue
        fi

        case "$bead_type" in
            task|bug|feature) ;;
            *) bead_type="task" ;;
        esac

        if [[ -z "$title" ]]; then
            continue
        fi

        # Build description with source attribution
        local full_description="$description"
        if [[ -n "$source_line" ]]; then
            full_description+=$'\n\n---\n'"**Source:** ${source_line}"
        fi
        if [[ -n "$source_file" ]]; then
            full_description+=$'\n'"**File:** ${source_file}"
        fi
        if [[ -n "$source" ]]; then
            full_description+=$'\n\n---\n'"**Gap identified from:** ${source}"
        fi

        # Build labels array
        local labels=("weave-generated" "from-docs")

        # Add verification_cmd as a label if present (format: verification_cmd:<command>)
        if [[ -n "$verification_cmd" ]]; then
            labels+=("verification_cmd:${verification_cmd}")
        fi

        # Join labels with commas for br create
        local labels_arg
        labels_arg=$(IFS=,; echo "${labels[*]}")

        local bead_id
        bead_id=$(_needle_create_bead \
            --workspace "$workspace" \
            --title "$title" \
            --description "$full_description" \
            --priority "$priority" \
            --type "$bead_type" \
            --labels "$labels_arg" \
            --silent 2>/dev/null)

        if [[ $? -eq 0 ]] && [[ -n "$bead_id" ]]; then
            _needle_info "weave: created bead: $bead_id - $title"

            _needle_telemetry_emit "weave.bead_created" "info" \
                "bead_id=$bead_id" \
                "title=$title" \
                "source=$source" \
                "workspace=$workspace"

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

_needle_weave_stats() {
    local state_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR"
    local run_count=0
    local last_run="never"

    if [[ -d "$state_dir" ]]; then
        run_count=$(find "$state_dir" -name "weave_last_run_*" -type f 2>/dev/null | wc -l)
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

# Check if weave strand is enabled (opt-in — disabled by default)
# Usage: _needle_weave_is_enabled
# Returns: 0 if enabled, 1 if disabled
# Supports both simple (strands.weave: true) and nested
# (strands.weave.enabled: true) config formats.
_needle_weave_is_enabled() {
    local enabled
    enabled=$(get_config "strands.weave" "false" 2>/dev/null)
    case "$enabled" in
        true|True|TRUE|yes|Yes|YES|1)
            return 0
            ;;
        false|False|FALSE|no|No|NO|0)
            return 1
            ;;
        *)
            # strands.weave is a nested object — check strands.weave.enabled
            local nested_enabled
            nested_enabled=$(get_config "strands.weave.enabled" "false" 2>/dev/null)
            case "$nested_enabled" in
                true|True|TRUE|yes|Yes|YES|1)
                    return 0
                    ;;
                *)
                    return 1
                    ;;
            esac
            ;;
    esac
}

# Find documentation files in workspace
# Usage: _needle_weave_find_docs <workspace>
# Returns: Newline-separated list of doc file paths
_needle_weave_find_docs() {
    local workspace="$1"
    local max_files
    max_files=$(get_config "strands.weave.max_doc_files" "15" 2>/dev/null || echo "15")

    local doc_files
    doc_files=$(find "$workspace" \
        \( -name "README.md" -o -name "ROADMAP*" -o -name "TODO*" \
           -o -name "CHANGELOG*" -o -name "plan.md" -o -name "PLAN*" \
           -o -name "ARCHITECTURE*" -o -name "DESIGN*" -o -name "ADR-*" \
           -o -name "AGENTS.md" -o -name "CLAUDE.md" \) \
        -type f \
        -not -path "*/.beads/*" \
        -not -path "*/node_modules/*" \
        -not -path "*/.git/*" \
        -not -path "*/vendor/*" \
        2>/dev/null | head -n "$max_files")

    # Also check docs/ directory
    if [[ -d "$workspace/docs" ]]; then
        local docs_dir_files
        docs_dir_files=$(find "$workspace/docs" -name "*.md" -type f \
            -not -name "worker-starvation-*" \
            2>/dev/null | head -n 10)
        if [[ -n "$docs_dir_files" ]]; then
            doc_files=$(printf '%s\n%s' "$doc_files" "$docs_dir_files" | sort -u | head -n "$max_files")
        fi
    fi

    echo "$doc_files"
}

# Get open and in-progress beads for deduplication
# Usage: _needle_weave_get_open_beads <workspace>
# Returns: JSON array of bead title strings
_needle_weave_get_open_beads() {
    local workspace="$1"

    local beads
    beads=$(cd "$workspace" 2>/dev/null && br list --json 2>/dev/null)

    if [[ -z "$beads" ]] || [[ "$beads" == "null" ]] || [[ "$beads" == "[]" ]]; then
        echo "[]"
        return 0
    fi

    if _needle_command_exists jq; then
        echo "$beads" | jq -c '[.[] | select(.status == "open" or .status == "in_progress") | .title]' 2>/dev/null || echo "[]"
    else
        echo "[]"
    fi
}

_needle_weave_run() {
    local workspace="$1"
    local agent="${2:-default}"
    _needle_weave_clear_rate_limit "$workspace"
    _needle_strand_weave "$workspace" "$agent"
}
