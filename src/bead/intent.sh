#!/usr/bin/env bash
# NEEDLE Intent Declaration Module
# Proactive file reservation before agent execution begins
#
# This module provides:
# - Extract files from bead metadata (explicit files label)
# - Extract file paths from bead descriptions via regex
# - Claim with intent - reserve declared files before execution
# - Automatic dependency creation when file conflicts occur
#
# Design:
# - Files stored in label: "files:path1,path2,path3"
# - Description parsing: regex extracts common code file patterns
# - On claim: attempt to reserve all declared files
# - On conflict: create dependency and skip bead

# Source dependencies
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

if [[ -z "${_NEEDLE_LOCK_CHECKOUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lock/checkout.sh"
fi

if [[ -z "${_NEEDLE_JSON_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/json.sh"
fi

# Set module loaded flag
_NEEDLE_INTENT_LOADED=true

# ============================================================================
# Configuration
# ============================================================================

# Label prefix for storing file lists in bead metadata
NEEDLE_INTENT_FILES_LABEL="${NEEDLE_INTENT_FILES_LABEL:-files}"

# Config for enabling/disabling intent-based file reservation
NEEDLE_INTENT_ENABLED="${NEEDLE_INTENT_ENABLED:-true}"

# ============================================================================
# File Extraction Functions
# ============================================================================

# Extract file paths from bead description using regex
# Usage: _needle_extract_files_from_description <description>
# Returns: Newline-separated list of file paths
# Example:
#   files=$(_needle_extract_files_from_description "Fix bug in src/cli/run.sh")
#   # Returns: src/cli/run.sh
_needle_extract_files_from_description() {
    local description="$1"

    if [[ -z "$description" ]]; then
        return 0
    fi

    # Match common path patterns with file extensions
    # Pattern: paths ending in .ts, .js, .py, .sh, .rs, .go, .rb, .yaml, .yml, .json, .md, .txt
    # Handles:
    # - Absolute paths: /home/coder/project/src/file.sh
    # - Relative paths: src/lib/config.sh
    # - Paths in quotes or backticks
    # - Paths preceded by keywords like "file", "fix", "update"

    local files
    files=$(echo "$description" | grep -oE '([/a-zA-Z0-9_./-]+\.(ts|js|py|sh|rs|go|rb|yaml|yml|json|md|txt|toml|cfg|conf|ini|css|html|xml|nix|lua|java|kt|swift|cpp|c|h|hpp|cc|cxx))\b' | sort -u)

    echo "$files"
}

# Parse files label from bead metadata
# Usage: _needle_extract_files_from_label <bead_json>
# Returns: Comma-separated list of file paths (empty if none)
# Example:
#   files=$(_needle_extract_files_from_label "$bead_json")
_needle_extract_files_from_label() {
    local bead_json="$1"

    if [[ -z "$bead_json" ]]; then
        return 0
    fi

    if ! command -v jq &>/dev/null; then
        _needle_warn "jq not available, cannot extract files label"
        return 0
    fi

    # Look for labels starting with "files:"
    # Handle both array input and single object
    local files_label
    files_label=$(echo "$bead_json" | jq -r '
        (. // []) |
        if type == "array" then
            .[0].labels // []
        else
            .labels // []
        end |
        .[] |
        select(startswith("files:"))
    ' 2>/dev/null | head -1)

    if [[ -n "$files_label" ]]; then
        # Strip the "files:" prefix and return the rest
        echo "${files_label#files:}"
    fi
}

# Extract all declared files from a bead
# Usage: _needle_extract_files_from_bead <bead_id> [--workspace <workspace>]
# Returns: Newline-separated list of unique file paths
# Example:
#   files=$(_needle_extract_files_from_bead "nd-123" --workspace /home/coder/project)
_needle_extract_files_from_bead() {
    local bead_id=""
    local workspace=""
    local include_description="${NEEDLE_INTENT_EXTRACT_FROM_DESC:-true}"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --workspace)
                workspace="$2"
                shift 2
                ;;
            *)
                if [[ -z "$bead_id" ]]; then
                    bead_id="$1"
                fi
                shift
                ;;
        esac
    done

    if [[ -z "$bead_id" ]]; then
        _needle_error "extract_files_from_bead requires bead_id"
        return 1
    fi

    # Get bead JSON
    local bead_json
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        bead_json=$(cd "$workspace" && br show "$bead_id" --json 2>/dev/null)
    else
        bead_json=$(br show "$bead_id" --json 2>/dev/null)
    fi

    if [[ -z "$bead_json" ]]; then
        _needle_warn "Could not fetch bead $bead_id"
        return 1
    fi

    local files=()

    # 1. Extract from explicit files: label
    local label_files
    label_files=$(_needle_extract_files_from_label "$bead_json")

    if [[ -n "$label_files" ]]; then
        # Split comma-separated files
        while IFS=',' read -r file; do
            [[ -n "$file" ]] && files+=("$file")
        done <<< "$label_files"
    fi

    # 2. Extract from description if enabled
    if [[ "$include_description" == "true" ]]; then
        local description
        description=$(echo "$bead_json" | jq -r '.[0].description // ""' 2>/dev/null)

        if [[ -n "$description" ]]; then
            local desc_files
            desc_files=$(_needle_extract_files_from_description "$description")

            while IFS= read -r file; do
                [[ -n "$file" ]] && files+=("$file")
            done <<< "$desc_files"
        fi
    fi

    # 3. Return unique files
    if [[ ${#files[@]} -gt 0 ]]; then
        printf '%s\n' "${files[@]}" | sort -u
    fi
}

# ============================================================================
# Intent-Based Claim Functions
# ============================================================================

# Claim a bead with proactive file reservation
# Usage: _needle_claim_with_intent <bead_id> [--workspace <workspace>] [--actor <actor>]
# Returns:
#   0 - All files reserved, ready to proceed
#   1 - Conflict detected, dependency added, bead released
#   2 - No files to reserve (proceed normally)
# Example:
#   if _needle_claim_with_intent "nd-123" --actor worker-alpha; then
#       echo "All files reserved"
#   fi
_needle_claim_with_intent() {
    local bead_id=""
    local workspace=""
    local actor="${NEEDLE_WORKER:-${NEEDLE_SESSION:-unknown}}"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --workspace)
                workspace="$2"
                shift 2
                ;;
            --actor)
                actor="$2"
                shift 2
                ;;
            *)
                if [[ -z "$bead_id" ]]; then
                    bead_id="$1"
                fi
                shift
                ;;
        esac
    done

    if [[ -z "$bead_id" ]]; then
        _needle_error "claim_with_intent requires bead_id"
        return 1
    fi

    # Check if intent is enabled
    if [[ "$NEEDLE_INTENT_ENABLED" != "true" ]]; then
        _needle_debug "Intent-based reservation disabled, skipping"
        return 2
    fi

    # Extract declared files
    local files
    files=$(_needle_extract_files_from_bead "$bead_id" ${workspace:+--workspace "$workspace"})

    if [[ -z "$files" ]]; then
        _needle_debug "No files declared for bead $bead_id"
        return 2  # No files to reserve
    fi

    local file_count
    file_count=$(echo "$files" | wc -l)

    _needle_info "Bead $bead_id declares $file_count file(s), attempting reservation..."

    # Track reserved files for rollback
    local reserved_files=()

    # Try to reserve each file
    while IFS= read -r file; do
        [[ -z "$file" ]] && continue

        # Resolve to absolute path if relative
        local filepath="$file"
        if [[ "$file" != /* ]]; then
            filepath="${workspace:-$(pwd)}/$file"
        fi

        _needle_debug "Attempting to reserve: $filepath"

        # Try to checkout the file
        if checkout_file "$filepath" "$bead_id" "$actor"; then
            reserved_files+=("$filepath")
            _needle_debug "Reserved: $filepath"
        else
            # Conflict detected - read blocking bead info
            local blocking_info
            blocking_info=$(cat)

            local blocking_bead="unknown"
            if command -v jq &>/dev/null; then
                blocking_bead=$(echo "$blocking_info" | jq -r '.bead // "unknown"' 2>/dev/null)
            fi

            _needle_warn "File conflict: $filepath is held by $blocking_bead"

            # Rollback: release all files we reserved
            for reserved in "${reserved_files[@]}"; do
                release_file "$reserved" "$bead_id" 2>/dev/null || true
            done

            # Add dependency and release bead claim
            _needle_info "Adding dependency: $bead_id depends on $blocking_bead"

            if [[ -n "$workspace" && -d "$workspace" ]]; then
                (cd "$workspace" && br dep add "$bead_id" "$blocking_bead" 2>/dev/null) || true
            else
                br dep add "$bead_id" "$blocking_bead" 2>/dev/null || true
            fi

            # Release the bead back to open status
            _needle_info "Releasing bead $bead_id due to file conflict"

            # Release bead claim using SQL (to avoid CHECK constraint bug)
            local db_path="${workspace:-$(pwd)}/.beads/beads.db"
            if command -v sqlite3 &>/dev/null && [[ -f "$db_path" ]]; then
                sqlite3 "$db_path" \
                    "UPDATE issues SET status='open', assignee=NULL, claimed_by=NULL, claim_timestamp=NULL WHERE id='$bead_id';" 2>/dev/null || true
            fi

            # Emit conflict event
            if declare -f _needle_telemetry_emit &>/dev/null; then
                _needle_telemetry_emit "intent.conflict" "warn" \
                    "bead=$bead_id" \
                    "blocked_by=$blocking_bead" \
                    "file=$filepath"
            fi

            return 1  # Conflict, skip this bead
        fi
    done <<< "$files"

    _needle_success "Reserved $file_count file(s) for bead $bead_id"

    # Emit success event
    if declare -f _needle_telemetry_emit &>/dev/null; then
        _needle_telemetry_emit "intent.reserved" "info" \
            "bead=$bead_id" \
            "count=$file_count"
    fi

    return 0  # All files reserved
}

# ============================================================================
# Helper Functions
# ============================================================================

# Create a bead with files declaration
# Usage: _needle_create_bead_with_files [options] --files "file1,file2" -- <title>
# Returns: bead ID
# Example:
#   bead_id=$(_needle_create_bead_with_files --priority 1 --files "src/a.sh,src/b.sh" -- "Fix bug")
_needle_create_bead_with_files() {
    local files=""
    local workspace="${NEEDLE_WORKSPACE:-$(pwd)}"
    local br_args=()
    local title=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --workspace)
                workspace="$2"
                shift 2
                ;;
            --files|-f)
                files="$2"
                shift 2
                ;;
            --title)
                shift
                title="$1"
                br_args+=("--title" "$1")
                shift
                ;;
            --type|-t|--priority|-p|--description|-d|--labels|-l)
                br_args+=("$1" "$2")
                shift 2
                ;;
            --assignee|-a|--parent|--deps|--estimate|--due|--defer)
                br_args+=("$1" "$2")
                shift 2
                ;;
            --)
                shift
                title="$*"
                break
                ;;
            *)
                br_args+=("$1")
                shift
                ;;
        esac
    done

    # Add files as a label if provided
    if [[ -n "$files" ]]; then
        # Append to existing labels or create new
        local files_label="files:$files"
        br_args+=("--labels" "$files_label")
    fi

    # Create the bead
    local create_output
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        create_output=$(cd "$workspace" && br create "${br_args[@]}" 2>&1)
    else
        create_output=$(br create "${br_args[@]}" 2>&1)
    fi

    echo "$create_output" | grep -oP '(?:Created issue\s+)?[a-z]{2,}-[a-z0-9]+' | head -1
}

# Show intent information for a bead
# Usage: _needle_show_intent <bead_id> [--workspace <workspace>]
# Returns: JSON with intent information
# Example:
#   _needle_show_intent "nd-123" | jq .
_needle_show_intent() {
    local bead_id=""
    local workspace=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --workspace)
                workspace="$2"
                shift 2
                ;;
            *)
                bead_id="$1"
                shift
                ;;
        esac
    done

    if [[ -z "$bead_id" ]]; then
        echo '{"error":"bead_id required"}'
        return 1
    fi

    # Get bead JSON
    local bead_json
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        bead_json=$(cd "$workspace" && br show "$bead_id" --json 2>/dev/null)
    else
        bead_json=$(br show "$bead_id" --json 2>/dev/null)
    fi

    if [[ -z "$bead_json" ]]; then
        echo '{"error":"bead not found"}'
        return 1
    fi

    # Extract files
    local files
    files=$(_needle_extract_files_from_bead "$bead_id" ${workspace:+--workspace "$workspace"})

    # Build JSON output
    local files_json="["
    local first=true

    while IFS= read -r file; do
        [[ -z "$file" ]] && continue
        if [[ "$first" == "true" ]]; then
            files_json+="\"$file\""
            first=false
        else
            files_json+=",\"$file\""
        fi
    done <<< "$files"
    files_json+="]"

    # Check lock status for each file
    local lock_json="["
    first=true

    while IFS= read -r file; do
        [[ -z "$file" ]] && continue

        local filepath="$file"
        if [[ "$file" != /* ]]; then
            filepath="${workspace:-$(pwd)}/$file"
        fi

        local locked_by="null"
        if check_file "$filepath" 2>/dev/null; then
            local lock_info
            lock_info=$(cat 2>/dev/null)
            if command -v jq &>/dev/null; then
                locked_by=$(echo "$lock_info" | jq -c '{bead, worker}' 2>/dev/null || echo '{"bead":"unknown"}')
            fi
        fi

        if [[ "$first" == "true" ]]; then
            lock_json+="{\"path\":\"$file\",\"locked_by\":$locked_by}"
            first=false
        else
            lock_json+=",{\"path\":\"$file\",\"locked_by\":$locked_by}"
        fi
    done <<< "$files"
    lock_json+="]"

    # Build final JSON
    if command -v jq &>/dev/null; then
        jq -n \
            --argjson bead "$bead_json" \
            --argjson files "$files_json" \
            --argjson locks "$lock_json" \
            '{bead: $bead[0], files: $files, lock_status: $locks}'
    else
        echo "{\"bead\":$bead_json,\"files\":$files_json,\"lock_status\":$lock_json}"
    fi
}

# ============================================================================
# Module Exports
# ============================================================================

# Export functions for use in other modules
export -f _needle_extract_files_from_description
export -f _needle_extract_files_from_label
export -f _needle_extract_files_from_bead
export -f _needle_claim_with_intent
export -f _needle_create_bead_with_files
export -f _needle_show_intent

# Set module loaded flag (for source guards)
_NEEDLE_INTENT_LOADED=true

# ============================================================================
# Direct Execution Support (for testing)
# ============================================================================
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-help}" in
        extract)
            shift
            if [[ "${1:-}" == "--from-desc" ]]; then
                shift
                _needle_extract_files_from_description "${1:-}"
            elif [[ "${1:-}" == "--from-label" ]]; then
                shift
                local json
                json=$(br show "${1:-}" --json 2>/dev/null)
                _needle_extract_files_from_label "$json"
            else
                _needle_extract_files_from_bead "$@"
            fi
            ;;
        claim)
            shift
            _needle_claim_with_intent "$@"
            ;;
        show)
            shift
            _needle_show_intent "$@" | jq .
            ;;
        create)
            shift
            _needle_create_bead_with_files "$@"
            ;;
        *|help)
            echo "Usage: $0 <command> [options]"
            echo ""
            echo "Commands:"
            echo "  extract <bead_id>              Extract declared files from bead"
            echo "  extract --from-desc <text>     Extract files from description text"
            echo "  extract --from-label <bead_id> Extract files from bead label"
            echo "  claim <bead_id> [options]      Claim bead with file reservation"
            echo "  show <bead_id>                 Show intent information for bead"
            echo "  create [options]               Create bead with files declaration"
            echo ""
            echo "Options:"
            echo "  --workspace <path>             Workspace directory"
            echo "  --actor <name>                 Worker/actor name"
            echo "  --files <list>                 Comma-separated file list"
            echo ""
            echo "Environment:"
            echo "  NEEDLE_INTENT_ENABLED          Enable intent reservation (default: true)"
            ;;
    esac
fi
