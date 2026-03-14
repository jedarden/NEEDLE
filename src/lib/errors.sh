#!/usr/bin/env bash
# NEEDLE Error Handling Standardization Module
#
# Provides:
#   - Error type registry mapping exit codes to event types
#   - Validation that all errors emit JSONL events with required fields
#   - Error escalation logic (retry vs fail vs quarantine)
#   - Standard error handling functions
#
# Escalation actions:
#   retry      - Transient error; retry the operation
#   fail       - Persistent error; mark bead as failed
#   quarantine - Unrecoverable error; isolate bead from queue
#
# Usage:
#   source "$NEEDLE_SRC/lib/errors.sh"
#   _needle_error_handle "error.timeout" 6 "operation=claim_bead"
#   action=$(_needle_error_get_escalation "error.claim_failed")

_NEEDLE_ERRORS_LOADED=true

# Ensure _needle_telemetry_emit is available
if ! declare -f _needle_telemetry_emit &>/dev/null; then
    _NEEDLE_ERRORS_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    source "$_NEEDLE_ERRORS_SRC/../telemetry/events.sh"
fi

# Ensure get_config is available (for debug.auto_bead_* config)
if ! declare -f get_config &>/dev/null; then
    _NEEDLE_ERRORS_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    source "$_NEEDLE_ERRORS_SRC/config.sh"
fi

# ============================================================================
# Error Type Registry
# ============================================================================
# Maps error event types to: "<exit_code>:<escalation_action>"
# Escalation actions: retry | fail | quarantine
#
# Structure:
#   NEEDLE_ERROR_REGISTRY[<event_type>]="<exit_code>:<escalation>"
#
# Exit code conventions (from constants.sh):
#   0   = SUCCESS
#   1   = ERROR (general)
#   2   = USAGE
#   3   = CONFIG
#   4   = RUNTIME
#   5   = DEPENDENCY
#   6   = TIMEOUT
#   130 = CANCELLED (SIGINT)
#   137 = SIGKILL (agent crash / OOM)
#
declare -A NEEDLE_ERROR_REGISTRY=(
    # Claim errors - transient race conditions, safe to retry
    ["error.claim_failed"]="1:retry"

    # Agent errors - crash may be unrecoverable; quarantine to prevent loops
    ["error.agent_crash"]="137:quarantine"

    # Timeout - retry once, then escalate via retry count checks
    ["error.timeout"]="6:retry"

    # Workspace unavailable - runtime error, mark bead as failed
    ["error.workspace_unavailable"]="4:fail"

    # Configuration invalid - operator must fix config, fail the bead
    ["error.config_invalid"]="3:fail"

    # Missing dependency (npm/pip/cargo etc.) - may be fixable, retry
    ["error.dependency_missing"]="5:retry"

    # Rate limited by AI provider - transient, retry with backoff
    ["error.rate_limited"]="1:retry"

    # Budget exceeded - stop processing, quarantine (don't retry)
    ["error.budget_exceeded"]="1:quarantine"

    # Per-bead budget exceeded - quarantine this specific bead
    ["error.budget_per_bead_exceeded"]="1:quarantine"

    # General bead failure - fail the bead
    ["bead.failed"]="1:fail"

    # Hook failure - hook scripts failed, fail the bead
    ["hook.failed"]="1:fail"

    # Mitosis decomposition failed - retry (may succeed with different split)
    ["bead.mitosis.failed"]="1:retry"

    # File lock conflict - transient, retry
    ["error.file_conflict"]="1:retry"

    # Worker cancelled (SIGINT/SIGTERM) - do not retry
    ["error.worker_cancelled"]="130:fail"

    # Prompt build failed - configuration issue, fail the bead
    ["error.prompt_failed"]="1:fail"

    # Agent dispatch failed - runtime issue, retry
    ["error.dispatch_failed"]="4:retry"
)

# ============================================================================
# Registry Lookup Functions
# ============================================================================

# Get the exit code for a given error event type
# Usage: _needle_error_get_exit_code <event_type>
# Returns: exit code (integer), or 1 if not found in registry
_needle_error_get_exit_code() {
    local event_type="$1"
    local entry="${NEEDLE_ERROR_REGISTRY[$event_type]:-}"
    if [[ -z "$entry" ]]; then
        printf '1'
        return 1
    fi
    printf '%s' "${entry%%:*}"
}

# Get the escalation action for a given error event type
# Usage: _needle_error_get_escalation <event_type>
# Returns: "retry" | "fail" | "quarantine"
_needle_error_get_escalation() {
    local event_type="$1"
    local entry="${NEEDLE_ERROR_REGISTRY[$event_type]:-}"
    if [[ -z "$entry" ]]; then
        # Unknown error type defaults to "fail"
        printf 'fail'
        return 1
    fi
    printf '%s' "${entry##*:}"
}

# Check if an error event type is registered
# Usage: _needle_error_is_registered <event_type>
# Returns: 0 if registered, 1 if not
_needle_error_is_registered() {
    local event_type="$1"
    [[ -n "$event_type" ]] && [[ -n "${NEEDLE_ERROR_REGISTRY[$event_type]:-}" ]]
}

# List all registered error event types
# Usage: _needle_error_list_types
# Returns: newline-separated list of event types
_needle_error_list_types() {
    local key
    for key in "${!NEEDLE_ERROR_REGISTRY[@]}"; do
        printf '%s\n' "$key"
    done | sort
}

# ============================================================================
# JSONL Event Validation
# ============================================================================

# Validate that a JSONL event string has all required fields
# Required fields: ts, event, level, session, worker, data
# Usage: _needle_error_validate_jsonl_event <json_string>
# Returns: 0 if valid, 1 if invalid (prints errors to stderr)
_needle_error_validate_jsonl_event() {
    local json="$1"
    local valid=true
    local missing_fields=()

    if [[ -z "$json" ]]; then
        printf 'NEEDLE error validation: empty event string\n' >&2
        return 1
    fi

    if command -v jq &>/dev/null; then
        # Use jq for robust validation
        local required_fields=("ts" "event" "level" "session" "worker" "data")
        local field
        for field in "${required_fields[@]}"; do
            if ! echo "$json" | jq -e "has(\"$field\")" > /dev/null 2>&1; then
                missing_fields+=("$field")
                valid=false
            fi
        done

        # Validate ts is ISO8601 with milliseconds
        local ts
        ts=$(echo "$json" | jq -r '.ts // ""' 2>/dev/null)
        if [[ -n "$ts" ]] && ! [[ "$ts" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{3}Z$ ]]; then
            printf 'NEEDLE error validation: ts field has invalid format: %s\n' "$ts" >&2
            valid=false
        fi

        # Validate level is one of: debug, info, warn, error
        local level
        level=$(echo "$json" | jq -r '.level // ""' 2>/dev/null)
        if [[ -n "$level" ]] && ! [[ "$level" =~ ^(debug|info|warn|error)$ ]]; then
            printf 'NEEDLE error validation: level field has invalid value: %s\n' "$level" >&2
            valid=false
        fi

        # Validate data is an object (not null, string, etc.)
        local data_type
        data_type=$(echo "$json" | jq -r '.data | type' 2>/dev/null)
        if [[ -n "$data_type" ]] && [[ "$data_type" != "object" ]]; then
            printf 'NEEDLE error validation: data field must be an object, got: %s\n' "$data_type" >&2
            valid=false
        fi
    else
        # Fallback: basic string matching without jq
        local required_fields=("\"ts\"" "\"event\"" "\"level\"" "\"session\"" "\"worker\"" "\"data\"")
        local field
        for field in "${required_fields[@]}"; do
            if ! echo "$json" | grep -q "$field"; then
                missing_fields+=("$field")
                valid=false
            fi
        done
    fi

    if [[ ${#missing_fields[@]} -gt 0 ]]; then
        printf 'NEEDLE error validation: missing required fields: %s\n' "${missing_fields[*]}" >&2
    fi

    $valid
}

# Validate that a JSONL event string has an event type registered in the error registry
# Usage: _needle_error_validate_error_event <json_string>
# Returns: 0 if valid error event, 1 if not an error event or not registered
_needle_error_validate_error_event() {
    local json="$1"
    local event_type

    if command -v jq &>/dev/null; then
        event_type=$(echo "$json" | jq -r '.event // ""' 2>/dev/null)
    else
        event_type=$(echo "$json" | grep -oP '"event":"\K[^"]+' 2>/dev/null || echo "")
    fi

    if [[ -z "$event_type" ]]; then
        printf 'NEEDLE error validation: could not extract event type from JSON\n' >&2
        return 1
    fi

    # Only validate error-related events
    if [[ "$event_type" != error.* ]] && [[ "$event_type" != bead.failed ]] && \
       [[ "$event_type" != hook.failed ]] && [[ "$event_type" != bead.mitosis.failed ]]; then
        # Not an error event - skip registry check
        return 0
    fi

    if ! _needle_error_is_registered "$event_type"; then
        printf 'NEEDLE error validation: event type not in error registry: %s\n' "$event_type" >&2
        return 1
    fi

    return 0
}

# ============================================================================
# Error Escalation Logic
# ============================================================================

# Determine escalation action based on error type and retry count
# Applies retry limits: errors marked "retry" become "fail" after max retries
# Usage: _needle_error_escalation_with_retries <event_type> <retry_count> [max_retries]
# Returns: "retry" | "fail" | "quarantine"
_needle_error_escalation_with_retries() {
    local event_type="$1"
    local retry_count="${2:-0}"
    local max_retries="${3:-${NEEDLE_DEFAULT_RETRY_COUNT:-3}}"

    local base_action
    base_action=$(_needle_error_get_escalation "$event_type")

    case "$base_action" in
        retry)
            if [[ "$retry_count" -ge "$max_retries" ]]; then
                # Exceeded retry limit - escalate to fail
                printf 'fail'
            else
                printf 'retry'
            fi
            ;;
        fail|quarantine)
            # fail and quarantine are not affected by retry count
            printf '%s' "$base_action"
            ;;
        *)
            printf 'fail'
            ;;
    esac
}

# ============================================================================
# Standard Error Handler
# ============================================================================

# Handle an error by emitting a JSONL event and returning the escalation action
# This is the primary entry point for standardized error handling.
#
# Usage: _needle_error_handle <event_type> <exit_code> [key=value ...]
# Outputs: escalation action to stdout ("retry" | "fail" | "quarantine")
# Returns: 0 always (escalation action is in stdout, not return code)
#
# Example:
#   action=$(_needle_error_handle "error.timeout" 6 "operation=claim_bead" "bead_id=nd-123")
#   case "$action" in
#     retry)     ... ;;
#     fail)      ... ;;
#     quarantine) ... ;;
#   esac
_needle_error_handle() {
    local event_type="$1"
    local exit_code="${2:-1}"
    shift 2

    # Emit the JSONL event with exit_code included in data
    _needle_telemetry_emit "$event_type" "error" "exit_code=$exit_code" "$@"

    # Get the escalation action
    local escalation
    escalation=$(_needle_error_get_escalation "$event_type")

    # Auto-create bug bead for quarantine errors or unregistered error types
    # This is called after telemetry emit so the error is logged regardless
    _needle_error_auto_bead "$event_type" "$escalation" "exit_code=$exit_code" "$@" 2>/dev/null || true

    # Return the escalation action
    printf '%s' "$escalation"
}

# Handle an error with retry count tracking
# Usage: _needle_error_handle_with_retries <event_type> <exit_code> <retry_count> [key=value ...]
# Outputs: escalation action to stdout ("retry" | "fail" | "quarantine")
_needle_error_handle_with_retries() {
    local event_type="$1"
    local exit_code="${2:-1}"
    local retry_count="${3:-0}"
    shift 3

    # Emit the JSONL event
    _needle_telemetry_emit "$event_type" "error" \
        "exit_code=$exit_code" \
        "retry_count=$retry_count" \
        "$@"

    # Return escalation action respecting retry limits
    _needle_error_escalation_with_retries "$event_type" "$retry_count"
}

# ============================================================================
# Auto Bug Bead Creation
# ============================================================================

# Create a bug bead automatically when unexpected errors occur.
# Called from _needle_error_handle() when escalation is quarantine or error type
# is unregistered. Rate-limited by error signature to prevent bead floods.
#
# Usage: _needle_error_auto_bead <event_type> <escalation> [key=value ...]
# Returns: 0 (always, errors are logged but non-fatal)
#
# Example:
#   _needle_error_auto_bead "error.timeout" "quarantine" "bead_id=nd-123"
_needle_error_auto_bead() {
    local event_type="$1"
    local escalation="$2"
    shift 2

    # Check if auto bead creation is enabled
    local enabled
    enabled=$(get_config "debug.auto_bead_on_error" "false" 2>/dev/null)
    if [[ "$enabled" != "true" ]]; then
        return 0
    fi

    # Get configured workspace
    local workspace
    workspace=$(get_config "debug.auto_bead_workspace" "" 2>/dev/null)
    if [[ -z "$workspace" ]] || [[ ! -d "$workspace" ]]; then
        _needle_debug "auto_bead: workspace not configured or invalid, skipping"
        return 0
    fi

    # Check if br is available
    if ! command -v br &>/dev/null; then
        _needle_debug "auto_bead: br CLI not found, skipping"
        return 0
    fi

    # Get configured auto bead types
    local auto_types
    auto_types=$(get_config "debug.auto_bead_types" "quarantine,unregistered" 2>/dev/null)

    # Check if this error type should trigger auto bead creation
    local should_create=false

    # Check for quarantine escalation
    if [[ "$escalation" == "quarantine" ]]; then
        if [[ ",$auto_types," == *,quarantine,* ]]; then
            should_create=true
        fi
    fi

    # Check for unregistered error type
    if ! _needle_error_is_registered "$event_type"; then
        if [[ ",$auto_types," == *,unregistered,* ]]; then
            should_create=true
        fi
    fi

    if [[ "$should_create" != "true" ]]; then
        return 0
    fi

    # Rate limit check by error signature (event_type + workspace)
    local rate_limit
    rate_limit=$(get_config "debug.auto_bead_rate_limit" "3600" 2>/dev/null)

    local signature="${event_type}:$(basename "$workspace")"
    local signature_hash
    signature_hash=$(echo "$signature" | md5sum | cut -c1-8)

    local state_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR"
    local last_bead_file="$state_dir/auto_bead_${signature_hash}"

    mkdir -p "$state_dir"

    # Check rate limit
    if [[ -f "$last_bead_file" ]]; then
        local last_ts
        last_ts=$(cat "$last_bead_file" 2>/dev/null)

        if [[ -n "$last_ts" ]] && [[ "$last_ts" =~ ^[0-9]+$ ]]; then
            local now
            now=$(date +%s)
            local elapsed=$((now - last_ts))

            if ((elapsed < rate_limit)); then
                _needle_debug "auto_bead: rate limited (${elapsed}s since last bead, need ${rate_limit}s)"
                return 0
            fi
        fi
    fi

    # Record timestamp for rate limiting
    date +%s > "$last_bead_file"

    # Build bead title and body
    local title="[AUTO] ${event_type}: Unexpected error in $(basename "$workspace")"

    local body
    body="# Auto-generated Bug Report

## Error Event
- **Event Type**: \`${event_type}\`
- **Escalation**: \`${escalation}\`
- **Workspace**: \`${workspace}\`
- **Timestamp**: $(date -u +"%Y-%m-%dT%H:%M:%SZ")

## Context
"

    # Parse key=value pairs from arguments
    local context_parts=()
    while [[ $# -gt 0 ]]; do
        local kv="$1"
        if [[ "$kv" =~ ^([a-zA-Z_][a-zA-Z0-9_]*)=(.*)$ ]]; then
            local key="${BASH_REMATCH[1]}"
            local value="${BASH_REMATCH[2]}"
            context_parts+=("- **${key}**: \`${value}\`")
        fi
        shift
    done

    if [[ ${#context_parts[@]} -gt 0 ]]; then
        body+=$'\n'
        printf -v body '%s%s\n' "$body" "${context_parts[*]}"
    fi

    # Try to get worker log excerpt if available
    local worker_id="${NEEDLE_WORKER_ID:-unknown}"
    body+=$'\n'"## Worker Log (last 20 lines)

Worker: \`${worker_id}\`

\`\`\`
"
    local log_file="$NEEDLE_HOME/$NEEDLE_LOG_DIR/${worker_id}.log"
    if [[ -f "$log_file" ]]; then
        body+="$(tail -20 "$log_file" 2>/dev/null)"
    else
        body+="(log file not found)"
    fi
    body+=$'\n'"\\\`\`\`"

    # Create the bead using br CLI
    local bead_id
    bead_id=$(cd "$workspace" 2>/dev/null && br create \
        --type bug \
        --title "$title" \
        --description "$body" \
        --label "auto-generated" \
        --label "needle-error" \
        --status open \
        --silent 2>/dev/null)

    if [[ -n "$bead_id" ]] && [[ "$bead_id" != "null" ]]; then
        _needle_info "auto_bead: created bug bead ${bead_id} for ${event_type}"
    else
        _needle_warn "auto_bead: failed to create bead for ${event_type}"
    fi

    return 0
}

# ============================================================================
# Strand Error Consistency Helpers
# ============================================================================

# Assert that a JSONL log file contains only valid error events
# Scans log file for error.* events and validates each is registered + well-formed
# Usage: _needle_error_audit_log <log_file>
# Returns: 0 if all valid, 1 if any invalid events found
_needle_error_audit_log() {
    local log_file="$1"
    local invalid_count=0
    local checked=0

    if [[ ! -f "$log_file" ]]; then
        printf 'NEEDLE error audit: log file not found: %s\n' "$log_file" >&2
        return 1
    fi

    while IFS= read -r line; do
        [[ -z "$line" ]] && continue

        # Only audit lines that look like JSONL (start with {)
        [[ "$line" != "{"* ]] && continue

        local event_type=""
        if command -v jq &>/dev/null; then
            event_type=$(echo "$line" | jq -r '.event // ""' 2>/dev/null)
        else
            event_type=$(echo "$line" | grep -oP '"event":"\K[^"]+' 2>/dev/null || true)
        fi

        # Only validate error-category events
        if [[ "$event_type" != error.* ]] && [[ "$event_type" != "bead.failed" ]] && \
           [[ "$event_type" != "hook.failed" ]] && [[ "$event_type" != "bead.mitosis.failed" ]]; then
            continue
        fi

        ((checked++)) || true

        # Validate the event structure
        if ! _needle_error_validate_jsonl_event "$line" 2>/dev/null; then
            printf 'NEEDLE error audit: invalid event structure in %s: %s\n' "$log_file" "$event_type" >&2
            ((invalid_count++)) || true
        fi

        # Validate the event type is registered
        if ! _needle_error_is_registered "$event_type"; then
            printf 'NEEDLE error audit: unregistered error event type in %s: %s\n' "$log_file" "$event_type" >&2
            ((invalid_count++)) || true
        fi

    done < "$log_file"

    if [[ $invalid_count -gt 0 ]]; then
        printf 'NEEDLE error audit: %d invalid error event(s) found in %s (checked %d)\n' \
            "$invalid_count" "$log_file" "$checked" >&2
        return 1
    fi

    return 0
}
