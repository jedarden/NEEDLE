#!/usr/bin/env bash
# NEEDLE Prompt Builder Module
# Constructs the full prompt sent to agents when working on beads
#
# This module builds comprehensive prompts by assembling:
# - Bead title and description
# - Workspace context
# - Mentioned files extracted from description
# - Completion criteria and output format instructions

# Source dependencies (if not already loaded)
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

if [[ -z "${_NEEDLE_CONSTANTS_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/constants.sh"
fi

# Configuration for prompt building
NEEDLE_PROMPT_MAX_FILE_LINES="${NEEDLE_PROMPT_MAX_FILE_LINES:-100}"
NEEDLE_PROMPT_FILE_EXTENSIONS="${NEEDLE_PROMPT_FILE_EXTENSIONS:-py|js|ts|go|rs|sh|yaml|yml|json|md|txt|sql|proto|tf}"

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

    # Build additional context from mentioned files
    local context
    context=$(_needle_extract_file_context "$description" "$workspace")

    # Build the prompt
    _needle_format_prompt \
        --bead-id "$bead_id" \
        --title "$title" \
        --description "$description" \
        --labels "$labels" \
        --workspace "$workspace" \
        --priority "$priority" \
        --status "$status" \
        --type "$issue_type" \
        --context "$context"
}

# Extract file context from description
# Finds files mentioned in the description and includes their content
# Usage: _needle_extract_file_context <description> <workspace>
# Returns: Formatted file context section (may be empty)
_needle_extract_file_context() {
    local description="$1"
    local workspace="$2"
    local context=""

    # Extract file paths from description using configurable extensions
    local mentioned_files
    mentioned_files=$(echo "$description" | grep -oE "[a-zA-Z0-9_/.-]+\.($NEEDLE_PROMPT_FILE_EXTENSIONS)" 2>/dev/null | sort -u)

    if [[ -z "$mentioned_files" ]]; then
        echo ""
        return 0
    fi

    _needle_debug "Found mentioned files: $(echo "$mentioned_files" | tr '\n' ' ')"

    local files_found=false
    for file in $mentioned_files; do
        # Skip if file doesn't exist or is not readable
        local file_path="$workspace/$file"
        if [[ ! -f "$file_path" ]] || [[ ! -r "$file_path" ]]; then
            _needle_verbose "Skipping non-existent file: $file"
            continue
        fi

        if [[ "$files_found" == "false" ]]; then
            context+="### Mentioned Files\n\n"
            files_found=true
        fi

        # Include file content (limited to configured max lines)
        local file_content
        file_content=$(head -n "$NEEDLE_PROMPT_MAX_FILE_LINES" "$file_path" 2>/dev/null)

        if [[ -n "$file_content" ]]; then
            # Detect language from extension for syntax highlighting hint
            local lang
            lang=$(_needle_get_file_lang "$file")

            context+="#### \`$file\`\n"
            context+="\`\`\`$lang\n"
            context+="$file_content"
            context+="\n\`\`\`\n\n"
        fi
    done

    echo -e "$context"
}

# Get language identifier for syntax highlighting
# Usage: _needle_get_file_lang <filename>
# Returns: language identifier (e.g., "bash", "python", "json")
_needle_get_file_lang() {
    local filename="$1"
    local ext="${filename##*.}"

    case "$ext" in
        sh)     echo "bash" ;;
        py)     echo "python" ;;
        js)     echo "javascript" ;;
        ts)     echo "typescript" ;;
        go)     echo "go" ;;
        rs)     echo "rust" ;;
        yaml|yml) echo "yaml" ;;
        json)   echo "json" ;;
        md)     echo "markdown" ;;
        sql)    echo "sql" ;;
        proto)  echo "protobuf" ;;
        tf)     echo "hcl" ;;
        txt)    echo "" ;;
        *)      echo "$ext" ;;
    esac
}

# Format the final prompt
# Usage: _needle_format_prompt --bead-id <id> --title <title> ... --context <context>
# Returns: Formatted prompt string
_needle_format_prompt() {
    local bead_id="" title="" description="" labels="" workspace=""
    local priority="" status="" type="" context=""

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
            --context)      context="$2"; shift 2 ;;
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

    # Format context section
    local context_section=""
    if [[ -n "$context" ]]; then
        context_section="

## Additional Context
$context"
    fi

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
You are working on this task in the context of the workspace above.

Complete the task as described. Use the \`br\` CLI (\`~/.local/bin/br\`) to manage bead lifecycle:

### Workflow
1. Do the work described above
2. Commit your changes with a descriptive message referencing bead ID: ${bead_id}
3. Validate that your changes fully satisfy the bead requirements
4. If validated: \`br close ${bead_id}\`
5. If blocked or incomplete: \`br update ${bead_id} --status blocked\` and add a comment explaining why
6. If the bead requires creating sub-beads, create them and add as blockers

Use \`~/.local/bin/br --help\` and \`br <command> --help\` to understand available commands and options.

Exit with code 0 on success, non-zero on failure.
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
            echo ""
            echo "Environment variables:"
            echo "  NEEDLE_PROMPT_MAX_FILE_LINES  Max lines per file (default: 100)"
            echo "  NEEDLE_PROMPT_FILE_EXTENSIONS File extensions to include (regex)"
            ;;
        *)
            echo "Unknown command: ${1:-}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
fi
