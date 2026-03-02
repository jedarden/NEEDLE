#!/usr/bin/env bash
# NEEDLE Hook System Runner
# Execute user-defined scripts at lifecycle events
#
# This module provides a hook system that enables users to customize
# NEEDLE behavior without modifying core code. Hooks are user scripts
# that run at specific lifecycle events.
#
# Hook Exit Codes:
#   0 - Success (continue normally)
#   1 - Warning (log warning but continue)
#   2 - Abort (stop current operation)
#   3 - Skip (skip remaining hooks for this event)
#   124 - Timeout (hook exceeded timeout limit)
#
# Configuration (in ~/.needle/config.yaml):
#   hooks:
#     pre_claim: ~/.needle/hooks/pre-claim.sh
#     post_claim: ~/.needle/hooks/post-claim.sh
#     pre_execute: ~/.needle/hooks/pre-execute.sh
#     post_execute: ~/.needle/hooks/post-execute.sh
#     pre_complete: ~/.needle/hooks/pre-complete.sh
#     post_complete: ~/.needle/hooks/post-complete.sh
#     on_failure: ~/.needle/hooks/on-failure.sh
#     on_quarantine: ~/.needle/hooks/on-quarantine.sh
#     timeout: 30s
#     fail_action: warn  # warn | abort | ignore

# ============================================================================
# Hook Exit Codes
# ============================================================================

NEEDLE_HOOK_EXIT_SUCCESS=0
NEEDLE_HOOK_EXIT_WARNING=1
NEEDLE_HOOK_EXIT_ABORT=2
NEEDLE_HOOK_EXIT_SKIP=3
NEEDLE_HOOK_EXIT_TIMEOUT=124

# ============================================================================
# Supported Hook Types
# ============================================================================

NEEDLE_HOOK_TYPES=(
    "pre_claim"
    "post_claim"
    "pre_execute"
    "post_execute"
    "pre_complete"
    "post_complete"
    "on_failure"
    "on_quarantine"
)

# ============================================================================
# Hook Environment Setup
# ============================================================================

# Set up environment variables for hook execution
# Usage: _needle_set_hook_env <bead_id>
# Sets NEEDLE_* environment variables for the hook script
_needle_set_hook_env() {
    local bead_id="${1:-}"

    # Worker identity (already set by runner)
    export NEEDLE_WORKER="${NEEDLE_SESSION:-}"
    export NEEDLE_SESSION="${NEEDLE_SESSION:-}"
    export NEEDLE_PID="$$"

    # Bead identification
    export NEEDLE_BEAD_ID="$bead_id"
    export NEEDLE_BEAD_TITLE=""
    export NEEDLE_BEAD_PRIORITY=""
    export NEEDLE_BEAD_TYPE=""
    export NEEDLE_BEAD_LABELS=""

    # Populate bead details if we have a bead_id
    if [[ -n "$bead_id" ]]; then
        # Try to get bead info from br show command
        local bead_json
        if _needle_command_exists br; then
            bead_json=$(br show "$bead_id" --json 2>/dev/null || echo "")
        fi

        if [[ -n "$bead_json" ]]; then
            # Extract bead fields using jq if available
            if _needle_command_exists jq; then
                export NEEDLE_BEAD_TITLE=$(echo "$bead_json" | jq -r '.title // ""' 2>/dev/null)
                export NEEDLE_BEAD_PRIORITY=$(echo "$bead_json" | jq -r '.priority // 3' 2>/dev/null)
                export NEEDLE_BEAD_TYPE=$(echo "$bead_json" | jq -r '.type // "task"' 2>/dev/null)
                export NEEDLE_BEAD_LABELS=$(echo "$bead_json" | jq -r '.labels | join(",") // ""' 2>/dev/null)
            fi
        fi
    fi

    # Workspace and agent info
    export NEEDLE_WORKSPACE="${NEEDLE_WORKSPACE:-$(pwd)}"
    export NEEDLE_AGENT="${NEEDLE_AGENT:-}"
    export NEEDLE_STRAND="${NEEDLE_STRAND:-}"
    export NEEDLE_STRAND_NAME="${NEEDLE_STRAND_NAME:-}"

    # Execution context (set by post_execute)
    export NEEDLE_EXIT_CODE="${NEEDLE_EXIT_CODE:-}"
    export NEEDLE_DURATION_MS="${NEEDLE_DURATION_MS:-}"
    export NEEDLE_OUTPUT_FILE="${NEEDLE_OUTPUT_FILE:-}"

    # Hook-specific context
    export NEEDLE_HOOK_CONFIG_FILE="$NEEDLE_CONFIG_FILE"
    export NEEDLE_HOOK_HOME="$NEEDLE_HOME"
}

# ============================================================================
# Hook Runner
# ============================================================================

# Run a user-defined hook script
# Usage: _needle_run_hook <hook_name> [bead_id]
# Returns:
#   0 - Hook succeeded or no hook configured
#   1 - Hook requested abort
#   2 - Hook requested skip
_needle_run_hook() {
    local hook_name="$1"
    local bead_id="${2:-}"

    # Validate hook name
    local is_valid=false
    for valid_type in "${NEEDLE_HOOK_TYPES[@]}"; do
        if [[ "$hook_name" == "$valid_type" ]]; then
            is_valid=true
            break
        fi
    done

    if [[ "$is_valid" == "false" ]]; then
        _needle_warn "Unknown hook type: $hook_name"
        return 0
    fi

    # Get hook path from config
    local hook_path
    hook_path=$(get_config "hooks.$hook_name" "")

    # No hook configured
    if [[ -z "$hook_path" ]]; then
        _needle_debug "No hook configured for: $hook_name"
        return 0
    fi

    # Expand ~ to home directory
    hook_path="${hook_path/#\~/$HOME}"

    # Check if hook file exists
    if [[ ! -f "$hook_path" ]]; then
        _needle_debug "Hook file not found: $hook_path"
        return 0
    fi

    # Make hook executable if needed
    if [[ ! -x "$hook_path" ]]; then
        _needle_debug "Making hook executable: $hook_path"
        chmod +x "$hook_path" 2>/dev/null || {
            _needle_warn "Failed to make hook executable: $hook_path"
        }
    fi

    # Get timeout and fail_action settings
    local timeout
    timeout=$(get_config "hooks.timeout" "30s")

    local fail_action
    fail_action=$(get_config "hooks.fail_action" "warn")

    # Remove 's' suffix from timeout for the timeout command
    local timeout_seconds="${timeout%s}"

    # Set up environment for hook
    _needle_set_hook_env "$bead_id"
    export NEEDLE_HOOK="$hook_name"

    # Emit hook.started event
    _needle_event_hook_started "$hook_name" "bead_id=$bead_id" "path=$hook_path"

    local start_time
    start_time=$(date +%s%3N)
    local exit_code

    _needle_debug "Running hook: $hook_name ($hook_path)"

    # Execute hook with timeout
    # Use a subshell to capture output without affecting current shell
    local hook_output
    if [[ "$NEEDLE_VERBOSE" == "true" ]]; then
        # In verbose mode, show output directly
        if timeout "$timeout_seconds" "$hook_path" 2>&1; then
            exit_code=0
        else
            exit_code=$?
        fi
    else
        # Capture output for debugging
        if hook_output=$(timeout "$timeout_seconds" "$hook_path" 2>&1); then
            exit_code=0
        else
            exit_code=$?
        fi

        # Log hook output in verbose mode
        if [[ -n "$hook_output" ]] && [[ "$NEEDLE_VERBOSE" == "true" ]]; then
            _needle_verbose "Hook output: $hook_output"
        fi
    fi

    local duration
    duration=$(($(date +%s%3N) - start_time))

    # Handle exit codes
    case "$exit_code" in
        "$NEEDLE_HOOK_EXIT_SUCCESS")
            _needle_debug "Hook $hook_name completed successfully (${duration}ms)"
            _needle_event_hook_completed "$hook_name" \
                "bead_id=$bead_id" \
                "exit_code=$exit_code" \
                "duration_ms=$duration" \
                "result=success"
            return 0
            ;;

        "$NEEDLE_HOOK_EXIT_WARNING")
            _needle_warn "Hook $hook_name returned warning (${duration}ms)"
            _needle_event_hook_completed "$hook_name" \
                "bead_id=$bead_id" \
                "exit_code=$exit_code" \
                "duration_ms=$duration" \
                "result=warning"
            return 0
            ;;

        "$NEEDLE_HOOK_EXIT_ABORT")
            _needle_error "Hook $hook_name requested abort (${duration}ms)"
            _needle_event_hook_failed "$hook_name" \
                "bead_id=$bead_id" \
                "exit_code=$exit_code" \
                "action=abort"
            return 1
            ;;

        "$NEEDLE_HOOK_EXIT_SKIP")
            _needle_debug "Hook $hook_name requested skip (${duration}ms)"
            _needle_event_hook_completed "$hook_name" \
                "bead_id=$bead_id" \
                "exit_code=$exit_code" \
                "duration_ms=$duration" \
                "result=skip"
            return 2
            ;;

        "$NEEDLE_HOOK_EXIT_TIMEOUT")
            _needle_error "Hook $hook_name timed out after ${timeout_seconds}s"
            _needle_event_hook_failed "$hook_name" \
                "bead_id=$bead_id" \
                "exit_code=$exit_code" \
                "action=timeout"

            case "$fail_action" in
                abort)
                    return 1
                    ;;
                warn)
                    _needle_warn "Hook timeout treated as warning (fail_action=warn)"
                    return 0
                    ;;
                ignore)
                    return 0
                    ;;
                *)
                    return 0
                    ;;
            esac
            ;;

        *)
            # Other failure exit codes
            _needle_warn "Hook $hook_name failed with exit code $exit_code (${duration}ms)"

            case "$fail_action" in
                abort)
                    _needle_error "Hook failure aborting (fail_action=abort)"
                    _needle_event_hook_failed "$hook_name" \
                        "bead_id=$bead_id" \
                        "exit_code=$exit_code" \
                        "action=abort"
                    return 1
                    ;;
                warn)
                    _needle_warn "Hook failure treated as warning (fail_action=warn)"
                    _needle_event_hook_failed "$hook_name" \
                        "bead_id=$bead_id" \
                        "exit_code=$exit_code" \
                        "action=warn"
                    return 0
                    ;;
                ignore)
                    _needle_debug "Hook failure ignored (fail_action=ignore)"
                    return 0
                    ;;
                *)
                    return 0
                    ;;
            esac
            ;;
    esac
}

# ============================================================================
# Convenience Functions for Lifecycle Hooks
# ============================================================================

# Run pre_claim hook
# Usage: _needle_hook_pre_claim [bead_id]
_needle_hook_pre_claim() {
    _needle_run_hook "pre_claim" "${1:-}"
}

# Run post_claim hook
# Usage: _needle_hook_post_claim [bead_id]
_needle_hook_post_claim() {
    _needle_run_hook "post_claim" "${1:-}"
}

# Run pre_execute hook
# Usage: _needle_hook_pre_execute [bead_id]
_needle_hook_pre_execute() {
    _needle_run_hook "pre_execute" "${1:-}"
}

# Run post_execute hook
# Usage: _needle_hook_post_execute [bead_id]
_needle_hook_post_execute() {
    _needle_run_hook "post_execute" "${1:-}"
}

# Run pre_complete hook
# Usage: _needle_hook_pre_complete [bead_id]
_needle_hook_pre_complete() {
    _needle_run_hook "pre_complete" "${1:-}"
}

# Run post_complete hook
# Usage: _needle_hook_post_complete [bead_id]
_needle_hook_post_complete() {
    _needle_run_hook "post_complete" "${1:-}"
}

# Run on_failure hook
# Usage: _needle_hook_on_failure [bead_id]
_needle_hook_on_failure() {
    _needle_run_hook "on_failure" "${1:-}"
}

# Run on_quarantine hook
# Usage: _needle_hook_on_quarantine [bead_id]
_needle_hook_on_quarantine() {
    _needle_run_hook "on_quarantine" "${1:-}"
}

# ============================================================================
# Hook Management Utilities
# ============================================================================

# List all configured hooks
# Usage: _needle_list_hooks
# Returns: JSON object with hook names and paths
_needle_list_hooks() {
    local hooks_json="{"
    local first=true

    for hook_type in "${NEEDLE_HOOK_TYPES[@]}"; do
        local hook_path
        hook_path=$(get_config "hooks.$hook_type" "")

        if [[ -n "$hook_path" ]]; then
            if [[ "$first" == "true" ]]; then
                first=false
            else
                hooks_json+=","
            fi

            # Check if hook file exists
            local exists="false"
            local expanded_path="${hook_path/#\~/$HOME}"
            if [[ -f "$expanded_path" ]]; then
                exists="true"
            fi

            hooks_json+="\"$hook_type\":{\"path\":\"$(_needle_json_escape "$hook_path")\",\"exists\":$exists}"
        fi
    done

    hooks_json+="}"

    echo "$hooks_json"
}

# Validate hook configuration
# Usage: _needle_validate_hooks
# Returns: 0 if all configured hooks are valid, 1 otherwise
_needle_validate_hooks() {
    local has_errors=false

    for hook_type in "${NEEDLE_HOOK_TYPES[@]}"; do
        local hook_path
        hook_path=$(get_config "hooks.$hook_type" "")

        if [[ -n "$hook_path" ]]; then
            local expanded_path="${hook_path/#\~/$HOME}"

            if [[ ! -f "$expanded_path" ]]; then
                _needle_warn "Hook file not found: $hook_type -> $hook_path"
                has_errors=true
            elif [[ ! -r "$expanded_path" ]]; then
                _needle_warn "Hook file not readable: $hook_type -> $hook_path"
                has_errors=true
            fi
        fi
    done

    # Validate timeout setting
    local timeout
    timeout=$(get_config "hooks.timeout" "30s")

    if [[ ! "$timeout" =~ ^[0-9]+s?$ ]]; then
        _needle_warn "Invalid hooks.timeout format: $timeout (expected: Ns)"
        has_errors=true
    fi

    # Validate fail_action setting
    local fail_action
    fail_action=$(get_config "hooks.fail_action" "warn")

    case "$fail_action" in
        warn|abort|ignore)
            # Valid
            ;;
        *)
            _needle_warn "Invalid hooks.fail_action: $fail_action (expected: warn, abort, or ignore)"
            has_errors=true
            ;;
    esac

    [[ "$has_errors" == "false" ]]
}

# Create a sample hook script
# Usage: _needle_create_sample_hook <hook_type> [path]
_needle_create_sample_hook() {
    local hook_type="$1"
    local path="${2:-$NEEDLE_HOME/hooks/${hook_type//_/-}.sh}"

    # Validate hook type
    local is_valid=false
    for valid_type in "${NEEDLE_HOOK_TYPES[@]}"; do
        if [[ "$hook_type" == "$valid_type" ]]; then
            is_valid=true
            break
        fi
    done

    if [[ "$is_valid" == "false" ]]; then
        _needle_error "Invalid hook type: $hook_type"
        _needle_info "Valid types: ${NEEDLE_HOOK_TYPES[*]}"
        return 1
    fi

    # Expand path
    path="${path/#\~/$HOME}"

    # Check if file already exists
    if [[ -f "$path" ]]; then
        _needle_warn "Hook file already exists: $path"
        return 1
    fi

    # Create directory if needed
    local dir
    dir=$(dirname "$path")
    if [[ ! -d "$dir" ]]; then
        mkdir -p "$dir" || {
            _needle_error "Failed to create directory: $dir"
            return 1
        }
    fi

    # Write sample hook content
    cat > "$path" << 'SAMPLE_HOOK'
#!/usr/bin/env bash
# NEEDLE Hook Script
#
# This hook is called at specific lifecycle events.
# Exit codes:
#   0 - Success (continue normally)
#   1 - Warning (log warning but continue)
#   2 - Abort (stop current operation)
#   3 - Skip (skip remaining hooks for this event)
#
# Environment variables available:
#   NEEDLE_HOOK       - Name of this hook
#   NEEDLE_BEAD_ID    - Bead ID (if applicable)
#   NEEDLE_BEAD_TITLE - Bead title
#   NEEDLE_WORKSPACE  - Current workspace path
#   NEEDLE_SESSION    - Worker session ID
#   NEEDLE_PID        - Current process ID

set -euo pipefail

# Log that hook was called
echo "Hook $NEEDLE_HOOK called at $(date -u +%Y-%m-%dT%H:%M:%SZ)"

# If a bead is being processed, log its details
if [[ -n "${NEEDLE_BEAD_ID:-}" ]]; then
    echo "  Bead: $NEEDLE_BEAD_ID"
    echo "  Title: ${NEEDLE_BEAD_TITLE:-}"
fi

# Return success
exit 0
SAMPLE_HOOK

    # Make executable
    chmod +x "$path"

    _needle_success "Created sample hook: $path"
    _needle_info "Edit the file to customize behavior"
    _needle_info "Add to config: hooks.$hook_type: $path"
}

# Get hook status summary
# Usage: _needle_hook_status
# Returns: Human-readable hook status
_needle_hook_status() {
    local timeout fail_action
    timeout=$(get_config "hooks.timeout" "30s")
    fail_action=$(get_config "hooks.fail_action" "warn")

    _needle_section "Hook Configuration"
    _needle_table_row "timeout" "$timeout"
    _needle_table_row "fail_action" "$fail_action"
    _needle_print ""

    local has_hooks=false
    for hook_type in "${NEEDLE_HOOK_TYPES[@]}"; do
        local hook_path
        hook_path=$(get_config "hooks.$hook_type" "")

        if [[ -n "$hook_path" ]]; then
            has_hooks=true
            local expanded_path="${hook_path/#\~/$HOME}"
            local status

            if [[ -f "$expanded_path" ]]; then
                if [[ -x "$expanded_path" ]]; then
                    status="${NEEDLE_COLOR_GREEN}✓ ready${NEEDLE_COLOR_RESET}"
                else
                    status="${NEEDLE_COLOR_YELLOW}⚠ not executable${NEEDLE_COLOR_RESET}"
                fi
            else
                status="${NEEDLE_COLOR_RED}✗ not found${NEEDLE_COLOR_RESET}"
            fi

            printf '  %-15s %s %s\n' "$hook_type" "$hook_path" "$status"
        fi
    done

    if [[ "$has_hooks" == "false" ]]; then
        _needle_print "  No hooks configured"
    fi
}
