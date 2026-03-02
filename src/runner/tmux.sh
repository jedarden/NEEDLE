#!/usr/bin/env bash
# NEEDLE Tmux Session Management
# Manages tmux sessions for self-invoking workers
#
# This module provides functions for:
# - Detecting if running inside tmux
# - Creating and managing detached tmux sessions
# - Self-invoking into tmux sessions
# - Session naming with pattern support and NATO identifiers
# - Session operations (attach, kill, list)

# -----------------------------------------------------------------------------
# Session Detection Functions
# -----------------------------------------------------------------------------

# Check if currently running inside a tmux session
# Returns: 0 if in tmux, 1 if not
# Usage: if _needle_in_tmux; then ...
_needle_in_tmux() {
    [[ -n "${TMUX:-}" ]]
}

# Check if a tmux session exists
# Arguments:
#   $1 - Session name
# Returns: 0 if session exists, 1 if not
# Usage: if _needle_session_exists "needle-worker"; then ...
_needle_session_exists() {
    local session="$1"

    if [[ -z "$session" ]]; then
        return 1
    fi

    tmux has-session -t "$session" 2>/dev/null
}

# Check if tmux is available
# Returns: 0 if tmux is installed, 1 if not
# Usage: if _needle_tmux_available; then ...
_needle_tmux_available() {
    command -v tmux &>/dev/null
}

# -----------------------------------------------------------------------------
# Session Naming Functions
# -----------------------------------------------------------------------------

# Generate a session name from a pattern
# Arguments:
#   $1 - Pattern (default: needle-{runner}-{provider}-{model}-{identifier})
#   $2 - Runner name
#   $3 - Provider name
#   $4 - Model name
#   $5 - Identifier
# Returns: Formatted session name
# Usage: _needle_generate_session_name "needle-{runner}-{provider}-{model}-{identifier}" "claude" "anthropic" "sonnet" "alpha"
_needle_generate_session_name() {
    local default_pattern='needle-{runner}-{provider}-{model}-{identifier}'
    local pattern="${1:-$default_pattern}"
    local runner="${2:-unknown}"
    local provider="${3:-unknown}"
    local model="${4:-unknown}"
    local identifier="${5:-alpha}"

    local name="$pattern"

    # Replace placeholders using native bash string replacement
    name="${name//\{runner\}/$runner}"
    name="${name//\{provider\}/$provider}"
    name="${name//\{model\}/$model}"
    name="${name//\{identifier\}/$identifier}"

    # Sanitize the name (tmux session names have restrictions)
    name=$(echo "$name" | tr -cd '[:alnum:]._-')

    echo "$name"
}

# Get the next available NATO alphabet identifier
# Arguments:
#   $1 - Runner name
#   $2 - Provider name
#   $3 - Model name
# Returns: First unused NATO identifier, or number if all used
# Usage: identifier=$(_needle_next_identifier "claude" "anthropic" "sonnet")
_needle_next_identifier() {
    local runner="$1"
    local provider="$2"
    local model="$3"

    local prefix="needle-$runner-$provider-$model-"

    # Find existing sessions with this prefix
    local existing
    existing=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep "^$prefix" || true)

    # Find first unused NATO name
    for name in "${NEEDLE_NATO_ALPHABET[@]}"; do
        if ! echo "$existing" | grep -q "^${prefix}${name}$"; then
            echo "$name"
            return 0
        fi
    done

    # All NATO names used, fall back to number
    local count
    count=$(echo "$existing" | grep -c . || echo "0")
    echo "$((count + 1))"
}

# Parse session name components
# Arguments:
#   $1 - Session name (format: needle-{runner}-{provider}-{model}-{identifier})
# Sets globals:
#   NEEDLE_SESSION_RUNNER
#   NEEDLE_SESSION_PROVIDER
#   NEEDLE_SESSION_MODEL
#   NEEDLE_SESSION_IDENTIFIER
# Returns: 0 on success, 1 if parsing failed
# Usage: _needle_parse_session_name "needle-claude-anthropic-sonnet-alpha"
_needle_parse_session_name() {
    local session="$1"

    # Reset globals
    NEEDLE_SESSION_RUNNER=""
    NEEDLE_SESSION_PROVIDER=""
    NEEDLE_SESSION_MODEL=""
    NEEDLE_SESSION_IDENTIFIER=""

    if [[ -z "$session" ]]; then
        return 1
    fi

    # Remove needle- prefix
    local rest="${session#needle-}"

    # Parse components (format: runner-provider-model-identifier)
    IFS='-' read -r runner provider model identifier <<< "$rest"

    if [[ -z "$runner" ]] || [[ -z "$provider" ]] || [[ -z "$model" ]]; then
        return 1
    fi

    NEEDLE_SESSION_RUNNER="$runner"
    NEEDLE_SESSION_PROVIDER="$provider"
    NEEDLE_SESSION_MODEL="$model"
    NEEDLE_SESSION_IDENTIFIER="${identifier:-alpha}"

    return 0
}

# -----------------------------------------------------------------------------
# Session Creation Functions
# -----------------------------------------------------------------------------

# Create a detached tmux session running a command
# Arguments:
#   $1 - Session name
#   $2... - Command to run
# Returns: 0 on success, 1 on failure
# Usage: _needle_create_session "needle-worker" "needle" "run" "--agent" "claude-anthropic-sonnet"
_needle_create_session() {
    local session="$1"
    shift
    local cmd="$*"

    if [[ -z "$session" ]]; then
        _needle_error "Session name is required"
        return 1
    fi

    if [[ -z "$cmd" ]]; then
        _needle_error "Command is required"
        return 1
    fi

    if ! _needle_tmux_available; then
        _needle_error "tmux is not available"
        return 1
    fi

    # Check if session already exists
    if _needle_session_exists "$session"; then
        _needle_warn "Session already exists: $session"
        return 1
    fi

    # Create detached session
    tmux new-session -d -s "$session" "$cmd"

    if [[ $? -eq 0 ]]; then
        _needle_debug "Created tmux session: $session"
        return 0
    else
        _needle_error "Failed to create tmux session: $session"
        return 1
    fi
}

# Self-invoke into a tmux session
# This is the main entry point for the self-invoking pattern
# Arguments:
#   $1 - Session name
#   $2... - Arguments to pass to self
# Environment:
#   NEEDLE_TMUX_FLAG - Set to indicate we're already in tmux (internal)
# Returns: 0 if session created (caller should exit), 1 if should run directly
# Usage:
#   if _needle_self_invoke_tmux "needle-worker" "$@"; then
#       exit 0  # Session created, we're done
#   fi
#   # Continue with normal execution (we're inside tmux now)
_needle_self_invoke_tmux() {
    local session="$1"
    shift

    # Check if we're already in tmux
    if _needle_in_tmux; then
        _needle_debug "Already running inside tmux"
        return 1
    fi

    # Check if --_tmux flag is set (internal flag for re-exec detection)
    if [[ "${NEEDLE_TMUX_FLAG:-}" == "true" ]]; then
        # We've been re-invoked inside tmux, run directly
        _needle_debug "Running as invoked tmux session"
        return 1
    fi

    # Check for explicit --_tmux flag in args (for the re-exec)
    local args=("$@")
    for arg in "${args[@]}"; do
        if [[ "$arg" == "--_tmux" ]]; then
            # This is the re-exec, run directly
            return 1
        fi
    done

    # Check if tmux is available
    if ! _needle_tmux_available; then
        _needle_warn "tmux not available, running in foreground"
        return 1
    fi

    # Build command to re-exec ourselves with --_tmux flag
    local cmd="$0 $* --_tmux"

    # Create session
    if _needle_create_session "$session" "$cmd"; then
        _needle_info "Worker started in tmux session: $session"
        _needle_info "Attach with: needle attach $session"
        return 0
    else
        _needle_warn "Failed to create tmux session, running in foreground"
        return 1
    fi
}

# Create a worker session with auto-generated name
# Arguments:
#   $1 - Runner name
#   $2 - Provider name
#   $3 - Model name
#   $4... - Arguments to pass to self
# Returns: 0 if session created, 1 otherwise
# Sets: NEEDLE_CREATED_SESSION with the session name
# Usage: _needle_create_worker_session "claude" "anthropic" "sonnet" "--workspace" "/home/coder/project"
_needle_create_worker_session() {
    local runner="$1"
    local provider="$2"
    local model="$3"
    shift 3
    local args=("$@")

    # Get next available identifier
    local identifier
    identifier=$(_needle_next_identifier "$runner" "$provider" "$model")

    # Generate session name
    local session
    session=$(_needle_generate_session_name "" "$runner" "$provider" "$model" "$identifier")

    # Store for caller reference
    NEEDLE_CREATED_SESSION="$session"

    # Self-invoke into tmux
    _needle_self_invoke_tmux "$session" "${args[@]}"
}

# -----------------------------------------------------------------------------
# Session Operations
# -----------------------------------------------------------------------------

# Attach to a tmux session
# Arguments:
#   $1 - Session name
#   $2 - Read-only mode (optional, "true" or "false", default: "false")
# Returns: 0 on success, 1 on failure
# Usage: _needle_attach_session "needle-worker"
# Usage: _needle_attach_session "needle-worker" "true"
_needle_attach_session() {
    local session="$1"
    local read_only="${2:-false}"

    if [[ -z "$session" ]]; then
        _needle_error "Session name is required"
        return 1
    fi

    if ! _needle_session_exists "$session"; then
        _needle_error "Session does not exist: $session"
        return 1
    fi

    if $read_only; then
        tmux attach-session -t "$session" -r
    else
        tmux attach-session -t "$session"
    fi
}

# Kill a tmux session
# Arguments:
#   $1 - Session name
# Returns: 0 on success, 1 on failure
# Usage: _needle_kill_session "needle-worker"
_needle_kill_session() {
    local session="$1"

    if [[ -z "$session" ]]; then
        _needle_error "Session name is required"
        return 1
    fi

    if ! _needle_session_exists "$session"; then
        _needle_debug "Session already gone: $session"
        return 0
    fi

    tmux kill-session -t "$session"

    if [[ $? -eq 0 ]]; then
        _needle_info "Killed session: $session"
        return 0
    else
        _needle_error "Failed to kill session: $session"
        return 1
    fi
}

# Send a command to a tmux session
# Arguments:
#   $1 - Session name
#   $2... - Command to send
# Returns: 0 on success, 1 on failure
# Usage: _needle_send_to_session "needle-worker" "echo 'Hello'"
_needle_send_to_session() {
    local session="$1"
    shift
    local cmd="$*"

    if [[ -z "$session" ]]; then
        _needle_error "Session name is required"
        return 1
    fi

    if ! _needle_session_exists "$session"; then
        _needle_error "Session does not exist: $session"
        return 1
    fi

    tmux send-keys -t "$session" "$cmd" Enter
}

# -----------------------------------------------------------------------------
# Session Listing Functions
# -----------------------------------------------------------------------------

# List all needle tmux sessions
# Returns: List of session names, one per line
# Usage: _needle_list_sessions
_needle_list_sessions() {
    tmux list-sessions -F '#{session_name}' 2>/dev/null | grep '^needle-' || true
}

# List sessions with details
# Returns: Session info in format: name|created|attached
# Usage: _needle_list_sessions_detailed
_needle_list_sessions_detailed() {
    tmux list-sessions -F '#{session_name}|#{session_created}|#{session_attached}' 2>/dev/null | \
        grep '^needle-' || true
}

# List sessions as JSON
# Returns: JSON array of session objects
# Usage: _needle_list_sessions_json
_needle_list_sessions_json() {
    local sessions
    sessions=$(_needle_list_sessions)

    if [[ -z "$sessions" ]]; then
        echo "[]"
        return 0
    fi

    local json="["
    local first=true

    while IFS= read -r session; do
        if [[ -z "$session" ]]; then
            continue
        fi

        # Get session info
        local created attached windows
        created=$(tmux display-message -t "$session" -p '#{session_created}' 2>/dev/null || echo "0")
        attached=$(tmux display-message -t "$session" -p '#{session_attached}' 2>/dev/null || echo "0")
        windows=$(tmux display-message -t "$session" -p '#{session_windows}' 2>/dev/null || echo "0")

        # Parse session name components
        local runner="" provider="" model="" identifier=""
        if _needle_parse_session_name "$session"; then
            runner="$NEEDLE_SESSION_RUNNER"
            provider="$NEEDLE_SESSION_PROVIDER"
            model="$NEEDLE_SESSION_MODEL"
            identifier="$NEEDLE_SESSION_IDENTIFIER"
        fi

        if [[ "$first" == "true" ]]; then
            first=false
        else
            json+=","
        fi

        json+=$(cat <<ENTRY
{
    "session": "$session",
    "runner": "$runner",
    "provider": "$provider",
    "model": "$model",
    "identifier": "$identifier",
    "created": $created,
    "attached": $attached,
    "windows": $windows
}
ENTRY
)
    done <<< "$sessions"

    json+="]"
    echo "$json"
}

# Count needle sessions
# Returns: Number of needle sessions
# Usage: _needle_count_sessions
_needle_count_sessions() {
    local sessions
    sessions=$(_needle_list_sessions)

    if [[ -z "$sessions" ]]; then
        echo "0"
        return 0
    fi

    echo "$sessions" | wc -l | tr -d ' '
}

# Count sessions for a specific agent
# Arguments:
#   $1 - Agent name (runner-provider-model)
# Returns: Number of matching sessions
# Usage: _needle_count_agent_sessions "claude-anthropic-sonnet"
_needle_count_agent_sessions() {
    local agent="$1"

    if [[ -z "$agent" ]]; then
        echo "0"
        return 0
    fi

    local sessions
    sessions=$(_needle_list_sessions)

    if [[ -z "$sessions" ]]; then
        echo "0"
        return 0
    fi

    echo "$sessions" | grep -c "^needle-$agent-" || echo "0"
}

# -----------------------------------------------------------------------------
# Session Utility Functions
# -----------------------------------------------------------------------------

# Get session info as JSON
# Arguments:
#   $1 - Session name
# Returns: JSON object with session info
# Usage: _needle_get_session_info "needle-worker"
_needle_get_session_info() {
    local session="$1"

    if [[ -z "$session" ]] || ! _needle_session_exists "$session"; then
        echo "{}"
        return 0
    fi

    local created attached windows
    created=$(tmux display-message -t "$session" -p '#{session_created}' 2>/dev/null || echo "0")
    attached=$(tmux display-message -t "$session" -p '#{session_attached}' 2>/dev/null || echo "0")
    windows=$(tmux display-message -t "$session" -p '#{session_windows}' 2>/dev/null || echo "0")

    # Parse session name components
    local runner="" provider="" model="" identifier=""
    if _needle_parse_session_name "$session"; then
        runner="$NEEDLE_SESSION_RUNNER"
        provider="$NEEDLE_SESSION_PROVIDER"
        model="$NEEDLE_SESSION_MODEL"
        identifier="$NEEDLE_SESSION_IDENTIFIER"
    fi

    cat <<EOF
{
    "session": "$session",
    "runner": "$runner",
    "provider": "$provider",
    "model": "$model",
    "identifier": "$identifier",
    "created": $created,
    "attached": $attached,
    "windows": $windows
}
EOF
}

# Check if a session is attached
# Arguments:
#   $1 - Session name
# Returns: 0 if attached, 1 if not
# Usage: if _needle_is_session_attached "needle-worker"; then ...
_needle_is_session_attached() {
    local session="$1"

    if [[ -z "$session" ]] || ! _needle_session_exists "$session"; then
        return 1
    fi

    local attached
    attached=$(tmux display-message -t "$session" -p '#{session_attached}' 2>/dev/null || echo "0")

    [[ "$attached" -gt 0 ]]
}

# Rename a session
# Arguments:
#   $1 - Old session name
#   $2 - New session name
# Returns: 0 on success, 1 on failure
# Usage: _needle_rename_session "needle-old" "needle-new"
_needle_rename_session() {
    local old_session="$1"
    local new_session="$2"

    if [[ -z "$old_session" ]] || [[ -z "$new_session" ]]; then
        _needle_error "Both old and new session names are required"
        return 1
    fi

    if ! _needle_session_exists "$old_session"; then
        _needle_error "Session does not exist: $old_session"
        return 1
    fi

    if _needle_session_exists "$new_session"; then
        _needle_error "Session already exists: $new_session"
        return 1
    fi

    tmux rename-session -t "$old_session" "$new_session"
}

# Display sessions in human-readable format
# Usage: _needle_show_sessions
_needle_show_sessions() {
    local sessions
    sessions=$(_needle_list_sessions)

    if [[ -z "$sessions" ]]; then
        _needle_info "No active needle sessions"
        return 0
    fi

    _needle_section "Active Needle Sessions"

    while IFS= read -r session; do
        if [[ -z "$session" ]]; then
            continue
        fi

        # Parse components
        local runner="" provider="" model="" identifier=""
        if _needle_parse_session_name "$session"; then
            runner="$NEEDLE_SESSION_RUNNER"
            provider="$NEEDLE_SESSION_PROVIDER"
            model="$NEEDLE_SESSION_MODEL"
            identifier="$NEEDLE_SESSION_IDENTIFIER"
        fi

        # Check if attached
        local attached_status="detached"
        if _needle_is_session_attached "$session"; then
            attached_status="attached"
        fi

        _needle_table_row "$session" "[$runner/$provider/$model/$identifier] ($attached_status)"
    done <<< "$sessions"
}
