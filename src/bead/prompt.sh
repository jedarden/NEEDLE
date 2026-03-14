#!/usr/bin/env bash
# NEEDLE Prompt Builder Module
# Constructs the full prompt sent to agents when working on beads
#
# This module builds comprehensive prompts by assembling:
# - Bead title and description
# - Workspace context
# - Project context via three-tier genesis plan fallback
# - Type-aware completion instructions

# Source dependencies (if not already loaded)
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

if [[ -z "${_NEEDLE_CONSTANTS_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/constants.sh"
fi

# Build a prompt for an agent to work on a bead
# Usage: _needle_build_prompt <bead_id> <workspace>
# Returns: Formatted prompt string
# Exit codes:
#   0 - Success
#   1 - Error (bead not found, invalid workspace, etc.)
#
# Example:
#   prompt=$(_needle_build_prompt "nd-100" "/home/user/project")
_needle_build_prompt() {
    local bead_id="$1"
    local workspace="$2"

    # Validate inputs
    if [[ -z "$bead_id" ]]; then
        _needle_error "Bead ID is required"
        return 1
    fi

    if [[ -z "$workspace" ]]; then
        _needle_error "Workspace path is required"
        return 1
    fi

    # Normalize workspace path
    if [[ ! -d "$workspace" ]]; then
        _needle_error "Workspace does not exist: $workspace"
        return 1
    fi

    # Get bead details from br CLI
    # NOTE: br show must run in workspace context to find bead
    local bead_json
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        bead_json=$(cd "$workspace" && br show "$bead_id" --json 2>/dev/null)
    else
        bead_json=$(br show "$bead_id" --json 2>/dev/null)
    fi

    if [[ -z "$bead_json" ]] || [[ "$bead_json" == "[]" ]] || [[ "$bead_json" == "null" ]]; then
        _needle_error "Bead not found: $bead_id"
        return 1
    fi

    # Parse bead details (handle array or single object)
    local bead_object
    if echo "$bead_json" | jq -e 'type == "array"' &>/dev/null; then
        bead_object=$(echo "$bead_json" | jq -c '.[0]')
    else
        bead_object="$bead_json"
    fi

    # Extract bead fields
    local title description labels priority status issue_type
    title=$(echo "$bead_object" | jq -r '.title // "Untitled Task"')
    description=$(echo "$bead_object" | jq -r '.description // ""')
    labels=$(echo "$bead_object" | jq -r '.labels | if type == "array" then join(", ") else . // "" end')
    priority=$(echo "$bead_object" | jq -r '.priority // 2')
    status=$(echo "$bead_object" | jq -r '.status // "open"')
    issue_type=$(echo "$bead_object" | jq -r '.issue_type // "task"')

    _needle_debug "Building prompt for bead: $bead_id (priority: $priority, status: $status)"

    # Build the prompt
    _needle_format_prompt \
        --bead-id "$bead_id" \
        --title "$title" \
        --description "$description" \
        --labels "$labels" \
        --workspace "$workspace" \
        --priority "$priority" \
        --status "$status" \
        --type "$issue_type"
}

# Walk the blocker/dependency chain upward to find a genesis bead and resolve its plan path
# Usage: _needle_find_genesis_plan <bead_id> <workspace>
# Returns: "<genesis_title>|<plan_path>" on stdout (plan_path may be empty)
# Exit codes:
#   0 - Genesis bead found (check stdout for title and plan path)
#   1 - No genesis bead found in chain
#
# The function only extracts what is explicitly in the genesis bead body.
# If the plan path is absent, the caller must handle discovery themselves.
#
# Example:
#   result=$(_needle_find_genesis_plan "nd-100" "/home/user/project")
#   genesis_title="${result%%|*}"
#   plan_path="${result#*|}"
_needle_find_genesis_plan() {
    local bead_id="$1"
    local workspace="$2"
    local max_depth=10
    local depth=0
    local current_id="$bead_id"
    # Track visited IDs to prevent cycles
    local visited=()

    while [[ $depth -lt $max_depth ]]; do
        # Cycle detection
        local already_visited=false
        for v in "${visited[@]:-}"; do
            if [[ "$v" == "$current_id" ]]; then
                already_visited=true
                break
            fi
        done
        if [[ "$already_visited" == "true" ]]; then
            _needle_debug "Cycle detected at bead: $current_id"
            break
        fi
        visited+=("$current_id")

        # Fetch the current bead
        local bead_json
        bead_json=$(cd "$workspace" && br show "$current_id" --json 2>/dev/null)

        if [[ -z "$bead_json" ]] || [[ "$bead_json" == "[]" ]] || [[ "$bead_json" == "null" ]]; then
            _needle_debug "Could not fetch bead: $current_id"
            break
        fi

        # Handle array or single object
        local bead_object
        if echo "$bead_json" | jq -e 'type == "array"' &>/dev/null; then
            bead_object=$(echo "$bead_json" | jq -c '.[0]')
        else
            bead_object="$bead_json"
        fi

        local issue_type
        issue_type=$(echo "$bead_object" | jq -r '.issue_type // ""')

        if [[ "$issue_type" == "genesis" ]]; then
            local genesis_title description plan_path=""
            genesis_title=$(echo "$bead_object" | jq -r '.title // ""')
            description=$(echo "$bead_object" | jq -r '.description // ""')

            # Look for explicit "Tied to plan: <path>" reference in the description
            local tied_line
            tied_line=$(echo "$description" | grep -m1 -i "Tied to plan:" | sed 's/.*[Tt]ied to plan:[[:space:]]*//')
            if [[ -n "$tied_line" ]]; then
                plan_path="${tied_line%$'\r'}"  # strip trailing carriage return if any
                plan_path="${plan_path%%[[:space:]]*([[:space:]])}"  # strip trailing whitespace
            fi

            _needle_debug "Found genesis bead: $current_id (plan_path='$plan_path')"
            printf '%s|%s\n' "$genesis_title" "$plan_path"
            return 0
        fi

        # Walk up: take the first dependency (blocked_by bead)
        local next_id
        next_id=$(echo "$bead_object" | jq -r '(.dependencies // []) | .[0].id // ""')

        if [[ -z "$next_id" ]]; then
            _needle_debug "No more dependencies from bead: $current_id"
            break
        fi

        current_id="$next_id"
        (( depth++ ))
    done

    _needle_debug "No genesis bead found in chain from: $bead_id"
    return 1
}


# Generate type-specific workflow instructions
# Usage: _needle_get_type_instructions <type> <bead_id>
# Returns: Type-specific instruction block string
_needle_get_type_instructions() {
    local type="$1"
    local bead_id="$2"

    # Common footer for all types
    local common_footer
    common_footer="
Use \`~/.local/bin/br --help\` and \`br <command> --help\` to understand available commands and options.

If blocked or incomplete: \`br update ${bead_id} --status blocked\` and add a comment explaining why.

Exit with code 0 on success, non-zero on failure."

    case "$type" in
        bug)
            cat <<INSTRUCTIONS
You are working on a **bug fix** task in the context of the workspace above.

### Workflow
1. Reproduce the issue described
2. Identify root cause
3. Fix the bug
4. Add a regression test covering the fix
5. Commit your changes with a descriptive message referencing bead ID: ${bead_id}
6. Validate that your changes fully satisfy the bead requirements
7. If validated: \`br close ${bead_id}\`
${common_footer}
INSTRUCTIONS
            ;;

        feature)
            cat <<INSTRUCTIONS
You are working on a **feature implementation** task in the context of the workspace above.

### Workflow
1. Review the project plan (if available from context section) for scope
2. Implement the feature as described
3. Add tests for new functionality
4. Commit your changes with a descriptive message referencing bead ID: ${bead_id}
5. Validate that your changes fully satisfy the bead requirements
6. If validated: \`br close ${bead_id}\`
${common_footer}
INSTRUCTIONS
            ;;

        refactor)
            cat <<INSTRUCTIONS
You are working on a **refactor** task in the context of the workspace above.

### Workflow
1. Understand current behavior — run existing tests first
2. Refactor as described without changing external behavior
3. Verify all existing tests still pass
4. Commit your changes with a descriptive message referencing bead ID: ${bead_id}
5. Validate that your changes fully satisfy the bead requirements
6. If validated: \`br close ${bead_id}\`
${common_footer}
INSTRUCTIONS
            ;;

        docs)
            cat <<INSTRUCTIONS
You are working on a **documentation** task in the context of the workspace above.

### Workflow
1. Review project plan (if available) for documentation conventions
2. Update or create documentation as described
3. Commit your changes with a descriptive message referencing bead ID: ${bead_id}
4. Validate that your changes fully satisfy the bead requirements
5. If validated: \`br close ${bead_id}\`
${common_footer}
INSTRUCTIONS
            ;;

        genesis)
            cat <<INSTRUCTIONS
You are working on a **genesis bead** — this is an orchestration task, not a coding task.

### Workflow
1. This is an orchestration bead — do not write code directly
2. Review the linked plan and assess phase completion status
3. Update the progress checklist in the genesis bead body
4. Create child beads for the next incomplete phase if none exist
5. If all phases complete: \`br close ${bead_id}\`
${common_footer}
INSTRUCTIONS
            ;;

        *)  # task, chore, or unknown types
            cat <<INSTRUCTIONS
You are working on this task in the context of the workspace above.

Complete the task as described. Use the \`br\` CLI (\`~/.local/bin/br\`) to manage bead lifecycle:

### Workflow
1. Do the work described above
2. Commit your changes with a descriptive message referencing bead ID: ${bead_id}
3. Validate that your changes fully satisfy the bead requirements
4. If validated: \`br close ${bead_id}\`
5. If the bead requires creating sub-beads, create them and add as blockers
${common_footer}
INSTRUCTIONS
            ;;
    esac
}

# Format the final prompt
# Usage: _needle_format_prompt --bead-id <id> --title <title> ...
# Returns: Formatted prompt string
_needle_format_prompt() {
    local bead_id="" title="" description="" labels="" workspace=""
    local priority="" status="" type=""

    # Parse named arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --bead-id)      bead_id="$2"; shift 2 ;;
            --title)        title="$2"; shift 2 ;;
            --description)  description="$2"; shift 2 ;;
            --labels)       labels="$2"; shift 2 ;;
            --workspace)    workspace="$2"; shift 2 ;;
            --priority)     priority="$2"; shift 2 ;;
            --status)       status="$2"; shift 2 ;;
            --type)         type="$2"; shift 2 ;;
            *)              shift ;;
        esac
    done

    # Get workspace name for context
    local workspace_name
    workspace_name=$(basename "$workspace")

    # Build priority label
    local priority_label
    case "$priority" in
        0) priority_label="P0 (critical)" ;;
        1) priority_label="P1 (high)" ;;
        2) priority_label="P2 (normal)" ;;
        3) priority_label="P3 (low)" ;;
        *) priority_label="P${priority}" ;;
    esac

    # Format labels section
    local labels_section=""
    if [[ -n "$labels" ]]; then
        labels_section="

## Labels
$labels"
    fi

    # Build three-tier project context section
    local context_section=""
    local genesis_result
    if genesis_result=$(_needle_find_genesis_plan "$bead_id" "$workspace" 2>/dev/null); then
        local genesis_title="${genesis_result%%|*}"
        local plan_path="${genesis_result#*|}"

        if [[ -n "$plan_path" ]]; then
            # Tier 1: Genesis bead found WITH an explicit plan path
            context_section="

## Project Context
This bead is part of: Genesis: ${genesis_title}
Project plan: ${plan_path}
Review the plan to understand scope, conventions, and how this task fits."
        else
            # Tier 2: Genesis bead found, but NO explicit plan path
            context_section="

## Project Context
This bead is part of: Genesis: ${genesis_title}
The genesis bead does not reference a plan document directly.

To understand the project's scope and conventions, discover the plan document by:
- Examining closed beads for references to planning/design documents: \`br list --status closed --json\`
- Searching git history for commits that introduced planning docs: \`git log --all --diff-filter=A --name-only --oneline\`
- Use closed bead descriptions and git commit messages to identify the plan document and understand project intent"
        fi
    else
        # Tier 3: No genesis bead in the chain
        context_section="

## Project Context
This is a standalone task with no linked project plan.

To understand conventions and intent for the files/components this task covers:
- Find closed beads related to the files/components this task covers: \`br list --status closed --json\`
- Cross-reference with git history for those artifacts: \`git log --oneline -- <relevant paths>\`
- Use the pattern of prior changes to understand conventions and intent"
    fi

    # Get type-specific instructions
    local instructions
    instructions=$(_needle_get_type_instructions "$type" "$bead_id")

    # Build and output the prompt
    cat <<PROMPT
# Task: ${title}

## Bead ID
${bead_id}

## Description
${description}
${labels_section}

## Workspace
\`${workspace}\` (${workspace_name})

## Priority
${priority_label}

## Instructions
${instructions}
${context_section}
PROMPT
}

# Escape a prompt string for safe shell embedding
# Usage: _needle_escape_prompt <prompt>
# Returns: Escaped prompt string (safe for single-quoting)
# Example:
#   escaped=$(_needle_escape_prompt "$prompt")
#   eval "prompt='$escaped'"
_needle_escape_prompt() {
    local prompt="$1"

    # Escape single quotes for safe shell embedding
    # Replace ' with '\'' (end quote, escaped quote, start quote)
    printf '%s' "$prompt" | sed "s/'/'\\\\''/g"
}

# Escape a prompt for JSON embedding
# Usage: _needle_escape_json <prompt>
# Returns: JSON-escaped prompt string
_needle_escape_json() {
    local prompt="$1"

    # Use jq for proper JSON escaping
    if command -v jq &>/dev/null; then
        printf '%s' "$prompt" | jq -Rs '.'
    else
        # Fallback: basic JSON escaping
        prompt="${prompt//\\/\\\\}"      # Backslash
        prompt="${prompt//\"/\\\"}"      # Double quote
        prompt="${prompt//$'\n'/\\n}"    # Newline
        prompt="${prompt//$'\r'/\\r}"    # Carriage return
        prompt="${prompt//$'\t'/\\t}"    # Tab
        printf '"%s"' "$prompt"
    fi
}

# Build a minimal prompt (for quick previews or logging)
# Usage: _needle_build_minimal_prompt <bead_id> <workspace>
# Returns: Minimal formatted prompt (no file contents)
_needle_build_minimal_prompt() {
    local bead_id="$1"
    local workspace="$2"

    # Get bead details
    local bead_json
    bead_json=$(br show "$bead_id" --json 2>/dev/null)

    if [[ -z "$bead_json" ]] || [[ "$bead_json" == "[]" ]]; then
        return 1
    fi

    # Handle array or single object
    local bead_object
    if echo "$bead_json" | jq -e 'type == "array"' &>/dev/null; then
        bead_object=$(echo "$bead_json" | jq -c '.[0]')
    else
        bead_object="$bead_json"
    fi

    local title description
    title=$(echo "$bead_object" | jq -r '.title // "Untitled"')
    description=$(echo "$bead_object" | jq -r '.description // ""' | head -c 200)

    cat <<PROMPT
# ${title}

Bead ID: ${bead_id}
Workspace: ${workspace}

${description}...
PROMPT
}

# Validate prompt output
# Usage: _needle_validate_prompt <prompt>
# Returns: 0 if valid, 1 if invalid
_needle_validate_prompt() {
    local prompt="$1"

    # Check minimum content
    if [[ -z "$prompt" ]]; then
        _needle_error "Prompt is empty"
        return 1
    fi

    # Check for required sections
    if [[ ! "$prompt" =~ "## Bead ID" ]]; then
        _needle_error "Prompt missing Bead ID section"
        return 1
    fi

    if [[ ! "$prompt" =~ "## Description" ]]; then
        _needle_error "Prompt missing Description section"
        return 1
    fi

    if [[ ! "$prompt" =~ "## Workspace" ]]; then
        _needle_error "Prompt missing Workspace section"
        return 1
    fi

    return 0
}

# Direct execution support (for testing)
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-}" in
        build)
            if [[ -z "${2:-}" ]] || [[ -z "${3:-}" ]]; then
                echo "Usage: $0 build <bead_id> <workspace>"
                exit 1
            fi
            _needle_build_prompt "$2" "$3"
            ;;
        escape)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 escape <prompt>"
                exit 1
            fi
            _needle_escape_prompt "$2"
            ;;
        escape-json)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 escape-json <prompt>"
                exit 1
            fi
            _needle_escape_json "$2"
            ;;
        minimal)
            if [[ -z "${2:-}" ]] || [[ -z "${3:-}" ]]; then
                echo "Usage: $0 minimal <bead_id> <workspace>"
                exit 1
            fi
            _needle_build_minimal_prompt "$2" "$3"
            ;;
        validate)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 validate <prompt>"
                exit 1
            fi
            _needle_validate_prompt "$2"
            ;;
        -h|--help)
            echo "Usage: $0 <command> [args]"
            echo ""
            echo "Commands:"
            echo "  build <bead_id> <workspace>  Build full prompt for bead"
            echo "  minimal <bead_id> <workspace> Build minimal prompt (no files)"
            echo "  escape <prompt>             Escape prompt for shell embedding"
            echo "  escape-json <prompt>        Escape prompt for JSON embedding"
            echo "  validate <prompt>           Validate prompt structure"
            ;;
        *)
            echo "Unknown command: ${1:-}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
fi
