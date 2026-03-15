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

    # Defense-in-depth: Never create auto-beads for test sessions
    # This prevents test errors from contaminating production workspaces
    # even if the config override is not properly applied (e.g., when running
    # specific test functions in isolation or via IDE execution).
    # Session patterns: test-*, needle-test-*, perf-*
    if [[ -n "${NEEDLE_SESSION:-}" ]]; then
        case "$NEEDLE_SESSION" in
            test-*|needle-test-*|perf-*)
                _needle_debug "auto_bead: skipping for test session: $NEEDLE_SESSION"
                return 0
                ;;
        esac
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

    # Get configured auto bead types (array from config, or comma-separated default)
    local auto_types_raw
    auto_types_raw=$(get_config "debug.auto_bead_types" "quarantine,unregistered" 2>/dev/null)

    # Check if this error type should trigger auto bead creation
    local should_create=false

    # Helper: check if a value is in the auto_types list
    _needle_auto_bead_type_enabled() {
        local check_type="$1"
        local types="$auto_types_raw"

        # If types is a JSON array string (starts with [), parse with jq or yq
        if [[ "$types" == \[* ]]; then
            if command -v jq &>/dev/null; then
                # Use jq to parse JSON array
                echo "$types" | jq -r '.[]' 2>/dev/null | grep -qx "$check_type"
                return $?
            elif command -v yq &>/dev/null; then
                # Use yq to parse JSON array (input as JSON string)
                echo "$types" | yq '.[]' 2>/dev/null | grep -qx "$check_type"
                return $?
            fi
        fi

        # If yq is available and types looks like a YAML array with dashes, use yq
        if command -v yq &>/dev/null && [[ "$types" == *"-"* ]]; then
            # Parse as YAML array using yq
            echo "$types" | yq '.[]' 2>/dev/null | grep -qx "$check_type"
            return $?
        fi

        # Fallback: treat as comma-separated string (for default or simple config)
        if [[ ",$types," == *,${check_type},* ]]; then
            return 0
        fi
        return 1
    }

    # Check for quarantine escalation
    if [[ "$escalation" == "quarantine" ]]; then
        if _needle_auto_bead_type_enabled "quarantine"; then
            should_create=true
        fi
    fi

    # Check for unregistered error type
    if ! _needle_error_is_registered "$event_type"; then
        if _needle_auto_bead_type_enabled "unregistered"; then
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

    local state_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR"
    local signatures_file="$state_dir/auto_bead_signatures.json"

    mkdir -p "$state_dir"

    # Initialize signatures file if it doesn't exist
    if [[ ! -f "$signatures_file" ]]; then
        echo "{}" > "$signatures_file"
    fi

    # Check rate limit using JSON file
    local last_ts=0
    if python3 -c "import json" 2>/dev/null; then
        last_ts=$(python3 - "$signatures_file" "$signature" 2>/dev/null <<'PYEOF'
import json, sys
with open(sys.argv[1]) as f:
    data = json.load(f)
print(data.get(sys.argv[2], 0))
PYEOF
        )
    fi

    if [[ -n "$last_ts" ]] && [[ "$last_ts" =~ ^[0-9]+$ ]]; then
        local now
        now=$(date +%s)
        local elapsed=$((now - last_ts))

        if ((elapsed >= rate_limit)); then
            # Rate limit expired, proceed with bead creation
            :
        else
            _needle_debug "auto_bead: rate limited (${elapsed}s since last bead, need ${rate_limit}s)"
            return 0
        fi
    fi

    # Record timestamp for rate limiting
    local now
    now=$(date +%s)
    if python3 -c "import json" 2>/dev/null; then
        python3 - "$signatures_file" "$signature" "$now" 2>/dev/null <<'PYEOF'
import json, sys
with open(sys.argv[1]) as f:
    data = json.load(f)
data[sys.argv[2]] = int(sys.argv[3])
with open(sys.argv[1], 'w') as f:
    json.dump(data, f)
PYEOF
    fi

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
        for _part in "${context_parts[@]}"; do
            body+="${_part}"$'\n'
        done
    fi

    # Try to get worker log excerpt if available
    # Use NEEDLE_SESSION for log file lookup (logs are named by session, not worker_id)
    local session_name="${NEEDLE_SESSION:-unknown}"
    local worker_id="${NEEDLE_WORKER_ID:-unknown}"
    body+=$'\n'"## Worker Log (last 20 lines)

Worker: \`${worker_id}\`
Session: \`${session_name}\`

\`\`\`
"

    # Worker logs are named ${NEEDLE_SESSION}.log, not ${NEEDLE_WORKER_ID}.log
    # Try NEEDLE_LOG_FILE first (set by runner), then construct from NEEDLE_LOG_DIR
    local log_file=""
    local log_content=""

    # Try NEEDLE_LOG_FILE first if set (this is the actual log file path used by the worker)
    # Regression fix for nd-w5bz: auto-bead was showing "(log file not found)" because
    # it was constructing the path from NEEDLE_LOG_DIR instead of using NEEDLE_LOG_FILE
    if [[ -n "${NEEDLE_LOG_FILE:-}" ]] && [[ -f "$NEEDLE_LOG_FILE" ]]; then
        log_content=$(tail -20 "$NEEDLE_LOG_FILE" 2>/dev/null)
        log_file="$NEEDLE_LOG_FILE"
    fi

    # Try worker log (${session}.log) constructed from NEEDLE_LOG_DIR
    if [[ -z "$log_content" ]] && [[ -n "$NEEDLE_LOG_DIR" ]] && [[ "$session_name" != "unknown" ]]; then
        local worker_log
        if [[ "$NEEDLE_LOG_DIR" = /* ]]; then
            # NEEDLE_LOG_DIR is already a full path
            worker_log="$NEEDLE_LOG_DIR/${session_name}.log"
        else
            # NEEDLE_LOG_DIR is a relative directory name
            worker_log="${NEEDLE_HOME:-$HOME/.needle}/$NEEDLE_LOG_DIR/${session_name}.log"
        fi
        if [[ -f "$worker_log" ]]; then
            log_content=$(tail -20 "$worker_log" 2>/dev/null)
            log_file="$worker_log"
        fi
    fi

    # Fall back to JSONL telemetry log (${session}.jsonl)
    if [[ -z "$log_content" ]] && [[ -n "$NEEDLE_LOG_DIR" ]] && [[ "$session_name" != "unknown" ]]; then
        local jsonl_log
        if [[ "$NEEDLE_LOG_DIR" = /* ]]; then
            # NEEDLE_LOG_DIR is already a full path
            jsonl_log="$NEEDLE_LOG_DIR/${session_name}.jsonl"
        else
            # NEEDLE_LOG_DIR is a relative directory name
            jsonl_log="${NEEDLE_HOME:-$HOME/.needle}/$NEEDLE_LOG_DIR/${session_name}.jsonl"
        fi
        if [[ -f "$jsonl_log" ]]; then
            # Show last 20 lines of JSONL log
            log_content=$(tail -20 "$jsonl_log" 2>/dev/null)
            log_file="$jsonl_log"
        fi
    fi

    if [[ -n "$log_content" ]]; then
        body+="$log_content"
    else
        body+="(log file not found)"
    fi
    body+=$'\n''```'

    # Create the bead using br CLI
    local bead_id
    bead_id=$(cd "$workspace" 2>/dev/null && br create \
        --type bug \
        --title "$title" \
        --description "$body" \
        --labels "auto-generated,needle-error" \
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
