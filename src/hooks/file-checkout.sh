#!/usr/bin/env bash
# NEEDLE File Checkout Hook
# preToolUse hook that intercepts Edit/Write operations and checks file locks
#
# This hook is called before Claude Code executes Edit or Write tools.
# It ensures that only one worker can edit a file at a time by using
# the /dev/shm-based file locking system.
#
# Exit Codes:
#   0 - File checkout successful (or not a file-editing tool)
#   1 - File locked by another bead (tool execution should be blocked)
#
# Environment Variables (set by NEEDLE runner):
#   NEEDLE_BEAD_ID   - Current bead ID
#   NEEDLE_WORKER    - Worker/session ID
#   NEEDLE_WORKSPACE - Current workspace path
#
# Tool Input (from stdin):
#   JSON object with file_path or path field
#
# On Conflict:
#   - Adds dependency to blocking bead via 'br dep add'
#   - Returns exit code 1 to signal NEEDLE to re-queue this bead

set -euo pipefail

# ============================================================================
# Configuration
# ============================================================================

# Path to NEEDLE lock module
NEEDLE_LOCK_MODULE="${NEEDLE_SRC_DIR:-${BASH_SOURCE[0]%/*/*}}/lock/checkout.sh"

# ============================================================================
# Logging Functions
# ============================================================================

_log_info() {
    echo "[INFO] $*" >&2
}

_log_warn() {
    echo "[WARN] $*" >&2
}

_log_error() {
    echo "[ERROR] $*" >&2
}

_log_debug() {
    [[ "${NEEDLE_VERBOSE:-}" == "true" ]] && echo "[DEBUG] $*" >&2
}

# ============================================================================
# Load Lock Module
# ============================================================================

# Source the lock checkout module
if [[ -f "$NEEDLE_LOCK_MODULE" ]]; then
    # shellcheck source=../lock/checkout.sh
    source "$NEEDLE_LOCK_MODULE"
else
    _log_error "Lock module not found: $NEEDLE_LOCK_MODULE"
    exit 1
fi

# ============================================================================
# Main Hook Logic
# ============================================================================

# Read tool input from stdin (Claude Code passes JSON via stdin)
TOOL_INPUT=""
if [[ -t 0 ]]; then
    # No stdin, try environment variable
    TOOL_INPUT="${TOOL_INPUT:-}"
else
    TOOL_INPUT=$(cat)
fi

# If no input, nothing to check
if [[ -z "$TOOL_INPUT" ]]; then
    _log_debug "No tool input, allowing execution"
    exit 0
fi

# Get tool name from environment (set by Claude Code hooks)
TOOL_NAME="${TOOL_NAME:-}"

# Only intercept Edit and Write tools
case "$TOOL_NAME" in
    Edit|Write)
        _log_debug "Intercepting $TOOL_NAME tool"
        ;;
    *)
        # Not a file-editing tool, allow execution
        _log_debug "Tool $TOOL_NAME does not require file checkout"
        exit 0
        ;;
esac

# Extract file path from JSON input
# Handle both 'file_path' (Edit tool) and 'path' (Write tool) field names
FILE_PATH=""

# Try to parse with jq if available
if command -v jq &>/dev/null; then
    FILE_PATH=$(echo "$TOOL_INPUT" | jq -r '.file_path // .path // empty' 2>/dev/null || true)
else
    # Fallback: grep for file_path or path
    FILE_PATH=$(echo "$TOOL_INPUT" | grep -oE '"(file_path|path)"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | sed 's/.*:[[:space:]]*"\([^"]*\)".*/\1/' 2>/dev/null || true)
fi

# If no file path found, allow execution
if [[ -z "$FILE_PATH" ]]; then
    _log_debug "No file path found in tool input, allowing execution"
    exit 0
fi

# Resolve to absolute path if relative
if [[ "$FILE_PATH" != /* ]]; then
    # Try to resolve relative to workspace or current directory
    if [[ -n "${NEEDLE_WORKSPACE:-}" ]]; then
        FILE_PATH="${NEEDLE_WORKSPACE}/${FILE_PATH}"
    else
        FILE_PATH="$(pwd)/${FILE_PATH}"
    fi
fi

_log_debug "Checking file: $FILE_PATH"

# Get bead and worker IDs
BEAD_ID="${NEEDLE_BEAD_ID:-}"
WORKER_ID="${NEEDLE_WORKER:-${NEEDLE_SESSION:-unknown}}"

# If no bead ID, we can't track locks - allow execution but warn
if [[ -z "$BEAD_ID" ]]; then
    _log_warn "No NEEDLE_BEAD_ID set, file checkout tracking disabled"
    exit 0
fi

# Attempt to checkout the file (capture output and status separately)
BLOCKING_INFO=$(checkout_file "$FILE_PATH" "$BEAD_ID" "$WORKER_ID")
CHECKOUT_STATUS=$?

if [[ $CHECKOUT_STATUS -eq 0 ]]; then
    _log_info "File checked out: $FILE_PATH"
    exit 0
fi

# File is locked by another bead
BLOCKING_BEAD=""

if command -v jq &>/dev/null; then
    BLOCKING_BEAD=$(echo "$BLOCKING_INFO" | jq -r '.bead // "unknown"' 2>/dev/null || echo "unknown")
else
    BLOCKING_BEAD=$(echo "$BLOCKING_INFO" | grep -oE '"bead"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*:[[:space:]]*"\([^"]*\)".*/\1/' 2>/dev/null || echo "unknown")
fi

_log_warn "FILE_CONFLICT: $FILE_PATH locked by $BLOCKING_BEAD"

# Add dependency to blocking bead so this bead will be re-queued when the other completes
if command -v br &>/dev/null; then
    _log_info "Adding dependency: $BEAD_ID depends on $BLOCKING_BEAD"
    br dep add "$BEAD_ID" "$BLOCKING_BEAD" 2>/dev/null || {
        _log_warn "Failed to add dependency (dependency may already exist)"
    }

    # Update bead status to open so it can be picked up later
    br update "$BEAD_ID" --status open 2>/dev/null || true
else
    _log_warn "br command not found, cannot add dependency"
fi

# Signal to NEEDLE that this bead should be re-queued
exit 1
