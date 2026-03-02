#!/usr/bin/env bash
# NEEDLE CLI Output Utilities
# Colored output and logging functions

# ANSI color codes
NEEDLE_COLOR_RESET='\033[0m'
NEEDLE_COLOR_RED='\033[0;31m'
NEEDLE_COLOR_GREEN='\033[0;32m'
NEEDLE_COLOR_YELLOW='\033[0;33m'
NEEDLE_COLOR_BLUE='\033[0;34m'
NEEDLE_COLOR_MAGENTA='\033[0;35m'
NEEDLE_COLOR_CYAN='\033[0;36m'
NEEDLE_COLOR_WHITE='\033[0;37m'
NEEDLE_COLOR_BOLD='\033[1m'
NEEDLE_COLOR_DIM='\033[2m'

# Color control flags
NEEDLE_USE_COLOR="${NEEDLE_USE_COLOR:-true}"

# Initialize color support
_needle_output_init() {
    # Check if colors should be disabled
    # NO_COLOR env var (https://no-color.org/) disables color by default
    if [[ -n "${NO_COLOR:-}" ]] || [[ "$NEEDLE_USE_COLOR" != "true" ]] || [[ ! -t 1 ]]; then
        NEEDLE_COLOR_RESET=''
        NEEDLE_COLOR_RED=''
        NEEDLE_COLOR_GREEN=''
        NEEDLE_COLOR_YELLOW=''
        NEEDLE_COLOR_BLUE=''
        NEEDLE_COLOR_MAGENTA=''
        NEEDLE_COLOR_CYAN=''
        NEEDLE_COLOR_WHITE=''
        NEEDLE_COLOR_BOLD=''
        NEEDLE_COLOR_DIM=''
    fi
}

# Output functions
_needle_print() {
    printf '%s\n' "$*"
}

_needle_print_color() {
    local color="$1"
    shift
    printf '%b%s%b\n' "$color" "$*" "$NEEDLE_COLOR_RESET"
}

_needle_info() {
    if [[ "${NEEDLE_QUIET:-}" == "true" ]]; then
        return
    fi
    _needle_print_color "$NEEDLE_COLOR_BLUE" "ℹ $*"
}

_needle_success() {
    if [[ "${NEEDLE_QUIET:-}" == "true" ]]; then
        return
    fi
    _needle_print_color "$NEEDLE_COLOR_GREEN" "✓ $*"
}

_needle_warn() {
    if [[ "${NEEDLE_QUIET:-}" == "true" ]]; then
        return
    fi
    _needle_print_color "$NEEDLE_COLOR_YELLOW" "⚠ $*" >&2
}

_needle_error() {
    _needle_print_color "$NEEDLE_COLOR_RED" "✗ $*" >&2
}

_needle_debug() {
    if [[ "${NEEDLE_VERBOSE:-false}" != "true" ]]; then
        return
    fi
    _needle_print_color "$NEEDLE_COLOR_DIM" "[DEBUG] $*"
}

_needle_verbose() {
    if [[ "${NEEDLE_VERBOSE:-false}" != "true" ]]; then
        return
    fi
    _needle_print_color "$NEEDLE_COLOR_CYAN" "  $*"
}

_needle_header() {
    if [[ "$NEEDLE_QUIET" == "true" ]]; then
        return
    fi
    _needle_print ""
    _needle_print_color "$NEEDLE_COLOR_BOLD$NEEDLE_COLOR_MAGENTA" "▌ $*"
    _needle_print_color "$NEEDLE_COLOR_DIM" "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
}

# Print a table row
_needle_table_row() {
    local col1="$1"
    local col2="${2:-}"
    if [[ -n "$col2" ]]; then
        printf '  %-20s %s\n' "$col1" "$col2"
    else
        printf '  %s\n' "$col1"
    fi
}

# Print a section header
_needle_section() {
    if [[ "$NEEDLE_QUIET" == "true" ]]; then
        return
    fi
    _needle_print ""
    _needle_print_color "$NEEDLE_COLOR_BOLD" "$*"
}
