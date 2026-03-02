#!/usr/bin/env bash
# NEEDLE CLI Utility Functions
# Common helper functions

# Check if a command exists
_needle_command_exists() {
    command -v "$1" &>/dev/null
}

# Get script directory
_needle_script_dir() {
    dirname "${BASH_SOURCE[0]}"
}

# Get NEEDLE root directory
_needle_root_dir() {
    local script="${BASH_SOURCE[0]}"
    while [[ -L "$script" ]]; do
        script=$(readlink "$script")
    done
    dirname "$(dirname "$script")"
}

# Join array elements with separator
_needle_join() {
    local sep="$1"
    shift
    local first="$1"
    shift
    printf '%s' "$first"
    for item in "$@"; do
        printf '%s%s' "$sep" "$item"
    done
}

# Check if running in interactive terminal
_needle_is_interactive() {
    [[ -t 0 && -t 1 ]]
}

# Prompt for confirmation
_needle_confirm() {
    local message="${1:-Continue?}"
    local default="${2:-n}"

    if ! _needle_is_interactive; then
        return 0
    fi

    local prompt
    if [[ "$default" == "y" ]]; then
        prompt="[Y/n]"
    else
        prompt="[y/N]"
    fi

    _needle_print_color "$NEEDLE_COLOR_YELLOW" "? ${message} ${prompt}"

    local response
    read -r response

    case "$response" in
        y|Y|yes|YES) return 0 ;;
        n|N|no|NO) return 1 ;;
        "") [[ "$default" == "y" ]] && return 0 || return 1 ;;
        *) return 1 ;;
    esac
}

# Sanitize string for use in filenames
_needle_sanitize() {
    echo "$1" | tr -cd '[:alnum:]._-' | tr '[:upper:]' '[:lower:]'
}

# Generate unique ID
_needle_generate_id() {
    local prefix="${1:-nd}"
    echo "${prefix}-$(date +%s%N | sha256sum | head -c 8)"
}

# Read file with fallback
_needle_read_file() {
    local file="$1"
    if [[ -f "$file" ]]; then
        cat "$file"
    else
        _needle_error "File not found: $file"
        return 1
    fi
}

# Write to file with directory creation
_needle_write_file() {
    local file="$1"
    local content="$2"
    local dir
    dir=$(dirname "$file")

    mkdir -p "$dir" && echo "$content" > "$file"
}

# Parse semver version string
_needle_parse_version() {
    local version="$1"
    local major minor patch

    IFS='.' read -r major minor patch <<< "$version"

    NEEDLE_PARSED_MAJOR="${major:-0}"
    NEEDLE_PARSED_MINOR="${minor:-0}"
    NEEDLE_PARSED_PATCH="${patch:-0}"
}

# Compare two versions (returns 0 if v1 <= v2, 1 otherwise)
_needle_version_compare() {
    local v1="$1"
    local v2="$2"

    _needle_parse_version "$v1"
    local maj1="$NEEDLE_PARSED_MAJOR"
    local min1="$NEEDLE_PARSED_MINOR"
    local pat1="$NEEDLE_PARSED_PATCH"

    _needle_parse_version "$v2"
    local maj2="$NEEDLE_PARSED_MAJOR"
    local min2="$NEEDLE_PARSED_MINOR"
    local pat2="$NEEDLE_PARSED_PATCH"

    if [[ $maj1 -lt $maj2 ]]; then return 0; fi
    if [[ $maj1 -gt $maj2 ]]; then return 1; fi
    if [[ $min1 -lt $min2 ]]; then return 0; fi
    if [[ $min1 -gt $min2 ]]; then return 1; fi
    if [[ $pat1 -le $pat2 ]]; then return 0; else return 1; fi
}
