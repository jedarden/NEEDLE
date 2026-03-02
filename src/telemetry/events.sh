#!/usr/bin/env bash
# NEEDLE CLI Telemetry Event Emitter
# Structured JSONL event emission for monitoring, analysis, and debugging

# ============================================================================
# Event Envelope Structure
# ============================================================================
# All events share a common structure:
# {
#   "ts": "2026-03-01T10:00:00.123Z",      # ISO8601 timestamp with milliseconds
#   "event": "bead.claimed",                # Event type (category.action)
#   "session": "needle-claude-anthropic-sonnet-alpha",  # Worker session ID
#   "worker": {
#     "runner": "claude",
#     "provider": "anthropic",
#     "model": "sonnet",
#     "identifier": "alpha"
#   },
#   "data": { ... }                         # Event-specific data
# }

# ============================================================================
# Worker Identity Environment Variables
# ============================================================================
# These should be set by the runner before sourcing this module:
#   NEEDLE_SESSION    - Unique session identifier (e.g., needle-claude-anthropic-sonnet-alpha)
#   NEEDLE_RUNNER     - Runner type (e.g., claude, cursor, aider)
#   NEEDLE_PROVIDER   - AI provider (e.g., anthropic, openai)
#   NEEDLE_MODEL      - Model identifier (e.g., sonnet, gpt-4)
#   NEEDLE_IDENTIFIER - Instance identifier (e.g., alpha, bravo, charlie)

# ============================================================================
# Core Event Emission Functions
# ============================================================================

# Build the worker identity JSON object
# Usage: _needle_telemetry_worker_json
# Returns: JSON object with runner, provider, model, identifier
_needle_telemetry_worker_json() {
    if _needle_command_exists jq; then
        jq -nc \
            --arg runner "${NEEDLE_RUNNER:-unknown}" \
            --arg provider "${NEEDLE_PROVIDER:-unknown}" \
            --arg model "${NEEDLE_MODEL:-unknown}" \
            --arg identifier "${NEEDLE_IDENTIFIER:-unknown}" \
            '{runner: $runner, provider: $provider, model: $model, identifier: $identifier}'
    else
        # Fallback: build JSON manually
        local runner provider model identifier
        runner=$(_needle_json_escape "${NEEDLE_RUNNER:-unknown}")
        provider=$(_needle_json_escape "${NEEDLE_PROVIDER:-unknown}")
        model=$(_needle_json_escape "${NEEDLE_MODEL:-unknown}")
        identifier=$(_needle_json_escape "${NEEDLE_IDENTIFIER:-unknown}")

        printf '{"runner":"%s","provider":"%s","model":"%s","identifier":"%s"}' \
            "$runner" "$provider" "$model" "$identifier"
    fi
}

# Get current ISO8601 timestamp with milliseconds
# Usage: _needle_telemetry_timestamp
# Returns: ISO8601 timestamp (e.g., 2026-03-01T10:00:00.123Z)
_needle_telemetry_timestamp() {
    date -u +%Y-%m-%dT%H:%M:%S.%3NZ 2>/dev/null || date -u +%Y-%m-%dT%H:%M:%S.000Z
}

# Build a data object from key=value arguments
# Handles string, number, boolean, and null values
# Usage: _needle_telemetry_build_data key1=value1 key2=value2 ...
# Returns: JSON object
_needle_telemetry_build_data() {
    if _needle_command_exists jq; then
        local data="{}"
        while [[ $# -gt 0 ]]; do
            if [[ "$1" == *=* ]]; then
                local key="${1%%=*}"
                local value="${1#*=}"

                # Detect value type and add to JSON
                if [[ "$value" =~ ^-?[0-9]+(\.[0-9]+)?$ ]]; then
                    # Number
                    data=$(echo "$data" | jq --arg k "$key" --argjson v "$value" '. + {($k): $v}')
                elif [[ "$value" == "true" ]] || [[ "$value" == "false" ]]; then
                    # Boolean
                    data=$(echo "$data" | jq --arg k "$key" --argjson v "$value" '. + {($k): $v}')
                elif [[ "$value" == "null" ]] || [[ -z "$value" ]]; then
                    # Null
                    data=$(echo "$data" | jq --arg k "$key" '. + {($k): null}')
                else
                    # String
                    data=$(echo "$data" | jq --arg k "$key" --arg v "$value" '. + {($k): $v}')
                fi
            fi
            shift
        done
        echo "$data"
    else
        # Fallback: build JSON manually using json.sh utilities
        local pairs=()
        while [[ $# -gt 0 ]]; do
            if [[ "$1" == *=* ]]; then
                pairs+=("$1")
            fi
            shift
        done
        _needle_json_object "${pairs[@]}"
    fi
}

# Emit a structured telemetry event
# This is the primary function for emitting events
# Usage: _needle_telemetry_emit <event_type> [key=value ...]
# Example: _needle_telemetry_emit "bead.claimed" "bead_id=nd-123" "workspace=/path"
_needle_telemetry_emit() {
    local event_type="$1"
    shift

    if [[ -z "$event_type" ]]; then
        _needle_warn "Cannot emit event: missing event type"
        return 1
    fi

    # Build event envelope
    local ts session worker data json

    ts=$(_needle_telemetry_timestamp)
    session="${NEEDLE_SESSION:-unknown}"
    worker=$(_needle_telemetry_worker_json)
    data=$(_needle_telemetry_build_data "$@")

    if _needle_command_exists jq; then
        # Build complete event JSON with jq
        json=$(jq -nc \
            --arg ts "$ts" \
            --arg event "$event_type" \
            --arg session "$session" \
            --argjson worker "$worker" \
            --argjson data "$data" \
            '{ts: $ts, event: $event, session: $session, worker: $worker, data: $data}')
    else
        # Fallback: build JSON manually
        json="{"
        json+="\"ts\":\"$(_needle_json_escape "$ts")\""
        json+=",\"event\":\"$(_needle_json_escape "$event_type")\""
        json+=",\"session\":\"$(_needle_json_escape "$session")\""
        json+=",\"worker\":$worker"
        json+=",\"data\":$data"
        json+="}"
    fi

    # Write to log file if writer is available and initialized
    if [[ -n "$NEEDLE_LOG_INITIALIZED" ]] && [[ "$NEEDLE_LOG_INITIALIZED" == "true" ]]; then
        _needle_write_event "$json"
    elif [[ -n "$NEEDLE_LOG_FILE" ]]; then
        # Log file is set but not initialized - try to append directly
        echo "$json" >> "$NEEDLE_LOG_FILE"
    fi

    # Print to stdout if verbose mode is enabled
    if [[ "${NEEDLE_VERBOSE:-}" == "true" ]]; then
        echo "$json"
    fi

    return 0
}

# ============================================================================
# Worker Events
# ============================================================================

# Emit worker.started event
# Usage: _needle_event_worker_started [key=value ...]
_needle_event_worker_started() {
    _needle_telemetry_emit "worker.started" "pid=$$" "$@"
}

# Emit worker.idle event
# Usage: _needle_event_worker_idle [key=value ...]
_needle_event_worker_idle() {
    _needle_telemetry_emit "worker.idle" "$@"
}

# Emit worker.stopped event
# Usage: _needle_event_worker_stopped [reason=...] [key=value ...]
_needle_event_worker_stopped() {
    _needle_telemetry_emit "worker.stopped" "pid=$$" "$@"
}

# Emit worker.draining event
# Usage: _needle_event_worker_draining [key=value ...]
_needle_event_worker_draining() {
    _needle_telemetry_emit "worker.draining" "$@"
}

# ============================================================================
# Bead Events
# ============================================================================

# Emit bead.claimed event
# Usage: _needle_event_bead_claimed <bead_id> [key=value ...]
_needle_event_bead_claimed() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.claimed" "bead_id=$bead_id" "$@"
}

# Emit bead.prompt_built event
# Usage: _needle_event_bead_prompt_built <bead_id> [key=value ...]
_needle_event_bead_prompt_built() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.prompt_built" "bead_id=$bead_id" "$@"
}

# Emit bead.agent_started event
# Usage: _needle_event_bead_agent_started <bead_id> [agent=...] [key=value ...]
_needle_event_bead_agent_started() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.agent_started" "bead_id=$bead_id" "$@"
}

# Emit bead.agent_completed event
# Usage: _needle_event_bead_agent_completed <bead_id> [result=...] [key=value ...]
_needle_event_bead_agent_completed() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.agent_completed" "bead_id=$bead_id" "$@"
}

# Emit bead.completed event
# Usage: _needle_event_bead_completed <bead_id> [result=...] [key=value ...]
_needle_event_bead_completed() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.completed" "bead_id=$bead_id" "$@"
}

# Emit bead.failed event
# Usage: _needle_event_bead_failed <bead_id> [error=...] [key=value ...]
_needle_event_bead_failed() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.failed" "bead_id=$bead_id" "$@"
}

# Emit bead.released event
# Usage: _needle_event_bead_released <bead_id> [reason=...] [key=value ...]
_needle_event_bead_released() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.released" "bead_id=$bead_id" "$@"
}

# ============================================================================
# Strand Events
# ============================================================================

# Emit strand.started event
# Usage: _needle_event_strand_started <bead_id> <strand> [key=value ...]
_needle_event_strand_started() {
    local bead_id="$1"
    local strand="$2"
    shift 2
    _needle_telemetry_emit "strand.started" "bead_id=$bead_id" "strand=$strand" "$@"
}

# Emit strand.fallthrough event
# Usage: _needle_event_strand_fallthrough <bead_id> <strand> [reason=...] [key=value ...]
_needle_event_strand_fallthrough() {
    local bead_id="$1"
    local strand="$2"
    shift 2
    _needle_telemetry_emit "strand.fallthrough" "bead_id=$bead_id" "strand=$strand" "$@"
}

# Emit strand.completed event
# Usage: _needle_event_strand_completed <bead_id> <strand> [result=...] [key=value ...]
_needle_event_strand_completed() {
    local bead_id="$1"
    local strand="$2"
    shift 2
    _needle_telemetry_emit "strand.completed" "bead_id=$bead_id" "strand=$strand" "$@"
}

# Emit strand.skipped event
# Usage: _needle_event_strand_skipped <bead_id> <strand> [reason=...] [key=value ...]
_needle_event_strand_skipped() {
    local bead_id="$1"
    local strand="$2"
    shift 2
    _needle_telemetry_emit "strand.skipped" "bead_id=$bead_id" "strand=$strand" "$@"
}

# ============================================================================
# Hook Events
# ============================================================================

# Emit hook.started event
# Usage: _needle_event_hook_started <hook_name> [key=value ...]
_needle_event_hook_started() {
    local hook_name="$1"
    shift
    _needle_telemetry_emit "hook.started" "hook=$hook_name" "$@"
}

# Emit hook.completed event
# Usage: _needle_event_hook_completed <hook_name> [result=...] [duration_ms=...] [key=value ...]
_needle_event_hook_completed() {
    local hook_name="$1"
    shift
    _needle_telemetry_emit "hook.completed" "hook=$hook_name" "$@"
}

# Emit hook.failed event
# Usage: _needle_event_hook_failed <hook_name> [error=...] [key=value ...]
_needle_event_hook_failed() {
    local hook_name="$1"
    shift
    _needle_telemetry_emit "hook.failed" "hook=$hook_name" "$@"
}

# ============================================================================
# Heartbeat Events
# ============================================================================

# Emit heartbeat.emitted event
# Usage: _needle_event_heartbeat_emitted [status=...] [key=value ...]
_needle_event_heartbeat_emitted() {
    _needle_telemetry_emit "heartbeat.emitted" "pid=$$" "$@"
}

# Emit heartbeat.stuck_detected event
# Usage: _needle_event_heartbeat_stuck_detected <stuck_session> [duration_seconds=...] [key=value ...]
_needle_event_heartbeat_stuck_detected() {
    local stuck_session="$1"
    shift
    _needle_telemetry_emit "heartbeat.stuck_detected" "stuck_session=$stuck_session" "$@"
}

# Emit heartbeat.recovery event
# Usage: _needle_event_heartbeat_recovery <recovered_session> [key=value ...]
_needle_event_heartbeat_recovery() {
    local recovered_session="$1"
    shift
    _needle_telemetry_emit "heartbeat.recovery" "recovered_session=$recovered_session" "$@"
}

# ============================================================================
# Error Events
# ============================================================================

# Emit error.claim_failed event
# Usage: _needle_event_error_claim_failed <bead_id> [reason=...] [key=value ...]
_needle_event_error_claim_failed() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "error.claim_failed" "bead_id=$bead_id" "$@"
}

# Emit error.agent_crash event
# Usage: _needle_event_error_agent_crash [agent=...] [error=...] [key=value ...]
_needle_event_error_agent_crash() {
    _needle_telemetry_emit "error.agent_crash" "pid=$$" "$@"
}

# Emit error.timeout event
# Usage: _needle_event_error_timeout <operation> [duration_seconds=...] [key=value ...]
_needle_event_error_timeout() {
    local operation="$1"
    shift
    _needle_telemetry_emit "error.timeout" "operation=$operation" "$@"
}

# ============================================================================
# Event Category Listing
# ============================================================================

# List all valid event types (for validation and documentation)
# Usage: _needle_telemetry_event_types
_needle_telemetry_event_types() {
    cat << 'EOF'
worker.started
worker.idle
worker.stopped
worker.draining
bead.claimed
bead.prompt_built
bead.agent_started
bead.agent_completed
bead.completed
bead.failed
bead.released
strand.started
strand.fallthrough
strand.completed
strand.skipped
hook.started
hook.completed
hook.failed
heartbeat.emitted
heartbeat.stuck_detected
heartbeat.recovery
error.claim_failed
error.agent_crash
error.timeout
EOF
}

# Check if an event type is valid
# Usage: _needle_telemetry_valid_event <event_type>
# Returns: 0 if valid, 1 if invalid
_needle_telemetry_valid_event() {
    local event_type="$1"
    _needle_telemetry_event_types | grep -qx "$event_type"
}
