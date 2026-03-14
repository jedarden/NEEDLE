#!/usr/bin/env bash
# NEEDLE CLI Telemetry Event Emitter
# Structured JSONL event emission for monitoring, analysis, and debugging

# ============================================================================
# Dependency Check
# ============================================================================
# Ensure _needle_command_exists is available (fallback if utils.sh not loaded)
if ! declare -f _needle_command_exists &>/dev/null; then
    _needle_command_exists() {
        command -v "$1" &>/dev/null
    }
fi

# Source FABRIC module for event forwarding (if available)
if [[ -z "${_NEEDLE_FABRIC_LOADED:-}" ]]; then
    _needle_fabric_path="$(dirname "${BASH_SOURCE[0]}")/fabric.sh"
    if [[ -f "$_needle_fabric_path" ]]; then
        source "$_needle_fabric_path"
        _NEEDLE_FABRIC_LOADED="true"
    fi
    unset _needle_fabric_path
fi

# Ensure _needle_json_escape is available (fallback if json.sh not loaded)
if ! declare -f _needle_json_escape &>/dev/null; then
    _needle_json_escape() {
        local str="$1"
        str="${str//\\/\\\\}"    # Escape backslashes first
        str="${str//\"/\\\"}"    # Escape double quotes
        str="${str//$'\n'/\\n}"  # Escape newlines
        str="${str//$'\r'/\\r}"  # Escape carriage returns
        str="${str//$'\t'/\\t}"  # Escape tabs
        printf '%s' "$str"
    }
fi

# Ensure _needle_json_object is available (fallback if json.sh not loaded)
if ! declare -f _needle_json_object &>/dev/null; then
    _needle_json_object() {
        local result="{"
        local first=true
        while [[ $# -gt 0 ]]; do
            if [[ "$1" == *=* ]]; then
                local key="${1%%=*}"
                local value="${1#*=}"
                if [[ "$first" != "true" ]]; then
                    result+=","
                fi
                first=false
                result+="\"$(_needle_json_escape "$key")\":\"$(_needle_json_escape "$value")\""
            fi
            shift
        done
        result+="}"
        printf '%s' "$result"
    }
fi

# ============================================================================
# Event Envelope Structure (NEEDLE-FABRIC Aligned)
# ============================================================================
# All events share a common structure:
# {
#   "ts": "2026-03-01T10:00:00.123Z",      # ISO8601 timestamp with milliseconds
#   "event": "bead.claimed",                # Event type (category.action)
#   "level": "info",                        # Log level: debug/info/warn/error
#   "session": "needle-claude-anthropic-sonnet-alpha",  # Worker session ID
#   "worker": "claude-anthropic-sonnet-alpha",  # Flat worker identity string
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

# Build the worker identity string (NEEDLE-FABRIC aligned format)
# Usage: _needle_telemetry_worker_string
# Returns: Flat string "${runner}-${provider}-${model}-${identifier}"
_needle_telemetry_worker_string() {
    local runner="${NEEDLE_RUNNER:-unknown}"
    local provider="${NEEDLE_PROVIDER:-unknown}"
    local model="${NEEDLE_MODEL:-unknown}"
    local identifier="${NEEDLE_IDENTIFIER:-unknown}"

    printf '%s-%s-%s-%s' "$runner" "$provider" "$model" "$identifier"
}

# Infer log level from event type (NEEDLE-FABRIC aligned)
# Usage: _needle_telemetry_infer_level <event_type>
# Returns: debug, info, warn, or error
# Rules:
#   - error.* -> error
#   - *.failed, *.retry -> warn
#   - debug.* -> debug
#   - default -> info
_needle_telemetry_infer_level() {
    local event_type="$1"

    # Error category events
    if [[ "$event_type" == error.* ]]; then
        printf 'error'
        return
    fi

    # Failed or retry events -> warn
    if [[ "$event_type" == *.failed ]] || [[ "$event_type" == *.retry ]]; then
        printf 'warn'
        return
    fi

    # Debug category events
    if [[ "$event_type" == debug.* ]]; then
        printf 'debug'
        return
    fi

    # Default to info
    printf 'info'
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
# Usage: _needle_telemetry_emit <event_type> [level] [key=value ...]
#   - event_type: Required. Event category.action (e.g., "bead.claimed")
#   - level: Optional. Log level (debug/info/warn/error). Auto-inferred if not provided.
#   - key=value: Optional data pairs
# Example: _needle_telemetry_emit "bead.claimed" "bead_id=nd-123" "workspace=/path"
# Example: _needle_telemetry_emit "error.timeout" "error" "operation=claim"
_needle_telemetry_emit() {
    local event_type="$1"
    shift

    if [[ -z "$event_type" ]]; then
        _needle_warn "Cannot emit event: missing event type"
        return 1
    fi

    # Determine level: check if first remaining arg is a valid level or infer from event type
    local level
    if [[ $# -gt 0 ]] && [[ "$1" =~ ^(debug|info|warn|error)$ ]]; then
        level="$1"
        shift
    else
        level=$(_needle_telemetry_infer_level "$event_type")
    fi

    # Build event envelope
    local ts session worker data json

    ts=$(_needle_telemetry_timestamp)
    session="${NEEDLE_SESSION:-unknown}"
    worker=$(_needle_telemetry_worker_string)
    data=$(_needle_telemetry_build_data "$@")

    if _needle_command_exists jq; then
        # Build complete event JSON with jq (worker is now a flat string)
        json=$(jq -nc \
            --arg ts "$ts" \
            --arg event "$event_type" \
            --arg level "$level" \
            --arg session "$session" \
            --arg worker "$worker" \
            --argjson data "$data" \
            '{ts: $ts, event: $event, level: $level, session: $session, worker: $worker, data: $data}')
    else
        # Fallback: build JSON manually (worker is now a string, not an object)
        json="{"
        json+="\"ts\":\"$(_needle_json_escape "$ts")\""
        json+=",\"event\":\"$(_needle_json_escape "$event_type")\""
        json+=",\"level\":\"$(_needle_json_escape "$level")\""
        json+=",\"session\":\"$(_needle_json_escape "$session")\""
        json+=",\"worker\":\"$(_needle_json_escape "$worker")\""
        json+=",\"data\":$data"
        json+="}"
    fi

    # Write to log file if writer is available and initialized
    if [[ -n "${NEEDLE_LOG_INITIALIZED:-}" ]] && [[ "$NEEDLE_LOG_INITIALIZED" == "true" ]]; then
        _needle_write_event "$json"
    elif [[ -n "${NEEDLE_LOG_FILE:-}" ]]; then
        # Log file is set but not initialized - try to append directly
        echo "$json" >> "$NEEDLE_LOG_FILE"
    fi

    # Print to stderr if verbose mode is enabled (stdout reserved for return values)
    if [[ "${NEEDLE_VERBOSE:-}" == "true" ]]; then
        echo "$json" >&2
    fi

    # Forward to FABRIC dashboard if enabled (non-blocking)
    # This enables live visualization of NEEDLE worker activity
    if declare -f _needle_fabric_forward_event &>/dev/null; then
        _needle_fabric_forward_event "$json"
    fi

    return 0
}

# ============================================================================
# Worker Events
# ============================================================================

# Emit worker.started event
# Usage: _needle_event_worker_started [key=value ...]
_needle_event_worker_started() {
    _needle_telemetry_emit "worker.started" "info" "pid=$$" "$@"
}

# Emit worker.idle event
# Usage: _needle_event_worker_idle [key=value ...]
_needle_event_worker_idle() {
    _needle_telemetry_emit "worker.idle" "info" "$@"
}

# Emit worker.stopped event
# Usage: _needle_event_worker_stopped [reason=...] [key=value ...]
_needle_event_worker_stopped() {
    _needle_telemetry_emit "worker.stopped" "info" "pid=$$" "$@"
}

# Emit worker.draining event
# Usage: _needle_event_worker_draining [key=value ...]
_needle_event_worker_draining() {
    _needle_telemetry_emit "worker.draining" "info" "$@"
}

# ============================================================================
# Bead Events
# ============================================================================

# Emit bead.claimed event
# Usage: _needle_event_bead_claimed <bead_id> [key=value ...]
_needle_event_bead_claimed() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.claimed" "info" "bead_id=$bead_id" "$@"
}

# Emit bead.prompt_built event
# Usage: _needle_event_bead_prompt_built <bead_id> [key=value ...]
_needle_event_bead_prompt_built() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.prompt_built" "info" "bead_id=$bead_id" "$@"
}

# Emit bead.agent_started event
# Usage: _needle_event_bead_agent_started <bead_id> [agent=...] [key=value ...]
_needle_event_bead_agent_started() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.agent_started" "info" "bead_id=$bead_id" "$@"
}

# Emit bead.agent_completed event
# Usage: _needle_event_bead_agent_completed <bead_id> [result=...] [key=value ...]
_needle_event_bead_agent_completed() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.agent_completed" "info" "bead_id=$bead_id" "$@"
}

# Emit bead.completed event
# Usage: _needle_event_bead_completed <bead_id> [result=...] [key=value ...]
_needle_event_bead_completed() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.completed" "info" "bead_id=$bead_id" "$@"
}

# Emit bead.failed event
# Usage: _needle_event_bead_failed <bead_id> [error=...] [key=value ...]
_needle_event_bead_failed() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.failed" "error" "bead_id=$bead_id" "$@"
}

# Emit bead.released event
# Usage: _needle_event_bead_released <bead_id> [reason=...] [key=value ...]
_needle_event_bead_released() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.released" "info" "bead_id=$bead_id" "$@"
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
    _needle_telemetry_emit "strand.started" "info" "bead_id=$bead_id" "strand=$strand" "$@"
}

# Emit strand.fallthrough event
# Usage: _needle_event_strand_fallthrough <bead_id> <strand> [reason=...] [key=value ...]
_needle_event_strand_fallthrough() {
    local bead_id="$1"
    local strand="$2"
    shift 2
    _needle_telemetry_emit "strand.fallthrough" "info" "bead_id=$bead_id" "strand=$strand" "$@"
}

# Emit strand.completed event
# Usage: _needle_event_strand_completed <bead_id> <strand> [result=...] [key=value ...]
_needle_event_strand_completed() {
    local bead_id="$1"
    local strand="$2"
    shift 2
    _needle_telemetry_emit "strand.completed" "info" "bead_id=$bead_id" "strand=$strand" "$@"
}

# Emit strand.skipped event
# Usage: _needle_event_strand_skipped <bead_id> <strand> [reason=...] [key=value ...]
_needle_event_strand_skipped() {
    local bead_id="$1"
    local strand="$2"
    shift 2
    _needle_telemetry_emit "strand.skipped" "info" "bead_id=$bead_id" "strand=$strand" "$@"
}

# ============================================================================
# Hook Events
# ============================================================================

# Emit hook.started event
# Usage: _needle_event_hook_started <hook_name> [key=value ...]
_needle_event_hook_started() {
    local hook_name="$1"
    shift
    _needle_telemetry_emit "hook.started" "info" "hook=$hook_name" "$@"
}

# Emit hook.completed event
# Usage: _needle_event_hook_completed <hook_name> [result=...] [duration_ms=...] [key=value ...]
_needle_event_hook_completed() {
    local hook_name="$1"
    shift
    _needle_telemetry_emit "hook.completed" "info" "hook=$hook_name" "$@"
}

# Emit hook.failed event
# Usage: _needle_event_hook_failed <hook_name> [error=...] [key=value ...]
_needle_event_hook_failed() {
    local hook_name="$1"
    shift
    _needle_telemetry_emit "hook.failed" "error" "hook=$hook_name" "$@"
}

# ============================================================================
# Heartbeat Events
# ============================================================================

# Emit heartbeat.emitted event
# Usage: _needle_event_heartbeat_emitted [status=...] [key=value ...]
_needle_event_heartbeat_emitted() {
    _needle_telemetry_emit "heartbeat.emitted" "debug" "pid=$$" "$@"
}

# Emit heartbeat.stuck_detected event
# Usage: _needle_event_heartbeat_stuck_detected <stuck_session> [duration_seconds=...] [key=value ...]
_needle_event_heartbeat_stuck_detected() {
    local stuck_session="$1"
    shift
    _needle_telemetry_emit "heartbeat.stuck_detected" "warn" "stuck_session=$stuck_session" "$@"
}

# Emit heartbeat.recovery event
# Usage: _needle_event_heartbeat_recovery <recovered_session> [key=value ...]
_needle_event_heartbeat_recovery() {
    local recovered_session="$1"
    shift
    _needle_telemetry_emit "heartbeat.recovery" "info" "recovered_session=$recovered_session" "$@"
}

# ============================================================================
# Mend Events (Maintenance Strand)
# ============================================================================

# Emit mend.orphan_released event
# Usage: _needle_event_mend_orphan_released <bead_id> [key=value ...]
_needle_event_mend_orphan_released() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "mend.orphan_released" "info" "bead_id=$bead_id" "$@"
}

# Emit mend.heartbeat_cleaned event
# Usage: _needle_event_mend_heartbeat_cleaned <worker> [key=value ...]
_needle_event_mend_heartbeat_cleaned() {
    local worker="$1"
    shift
    _needle_telemetry_emit "mend.heartbeat_cleaned" "info" "worker=$worker" "$@"
}

# Emit mend.logs_pruned event
# Usage: _needle_event_mend_logs_pruned [count=...] [key=value ...]
_needle_event_mend_logs_pruned() {
    _needle_telemetry_emit "mend.logs_pruned" "info" "$@"
}

# Emit mend.completed event
# Usage: _needle_event_mend_completed [key=value ...]
_needle_event_mend_completed() {
    _needle_telemetry_emit "mend.completed" "info" "$@"
}

# ============================================================================
# Unravel Events (Alternative Approaches for Blocked Beads)
# ============================================================================

# Emit unravel.alternatives_created event
# Usage: _needle_event_unravel_alternatives_created <parent_id> [alternatives_count=...] [key=value ...]
_needle_event_unravel_alternatives_created() {
    local parent_id="$1"
    shift
    _needle_telemetry_emit "unravel.alternatives_created" "info" "parent_id=$parent_id" "$@"
}

# Emit unravel.alternative_created event (individual alternative)
# Usage: _needle_event_unravel_alternative_created <parent_id> <alternative_id> [title=...] [key=value ...]
_needle_event_unravel_alternative_created() {
    local parent_id="$1"
    local alternative_id="$2"
    shift 2
    _needle_telemetry_emit "unravel.alternative_created" "info" "parent_id=$parent_id" "alternative_id=$alternative_id" "$@"
}

# Emit unravel.analysis_started event
# Usage: _needle_event_unravel_analysis_started [workspace=...] [human_bead_id=...] [key=value ...]
_needle_event_unravel_analysis_started() {
    _needle_telemetry_emit "unravel.analysis_started" "info" "$@"
}

# Emit unravel.analysis_completed event
# Usage: _needle_event_unravel_analysis_completed [alternatives_found=...] [key=value ...]
_needle_event_unravel_analysis_completed() {
    _needle_telemetry_emit "unravel.analysis_completed" "info" "$@"
}

# ============================================================================
# Weave Events (Documentation Gap Detection)
# ============================================================================

# Emit weave.bead_created event
# Usage: _needle_event_weave_bead_created <bead_id> [source=...] [key=value ...]
_needle_event_weave_bead_created() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "weave.bead_created" "info" "bead_id=$bead_id" "$@"
}

# Emit weave.analysis_started event
# Usage: _needle_event_weave_analysis_started [workspace=...] [doc_count=...] [key=value ...]
_needle_event_weave_analysis_started() {
    _needle_telemetry_emit "weave.analysis_started" "info" "$@"
}

# Emit weave.analysis_completed event
# Usage: _needle_event_weave_analysis_completed [gaps_found=...] [beads_created=...] [key=value ...]
_needle_event_weave_analysis_completed() {
    _needle_telemetry_emit "weave.analysis_completed" "info" "$@"
}

# ============================================================================
# Pulse Events (Codebase Health Monitoring)
# ============================================================================

# Emit pulse.bead_created event
# Usage: _needle_event_pulse_bead_created <bead_id> [category=...] [severity=...] [key=value ...]
_needle_event_pulse_bead_created() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "pulse.bead_created" "info" "bead_id=$bead_id" "$@"
}

# Emit pulse.scan_started event
# Usage: _needle_event_pulse_scan_started [workspace=...] [key=value ...]
_needle_event_pulse_scan_started() {
    _needle_telemetry_emit "pulse.scan_started" "info" "$@"
}

# Emit pulse.scan_completed event
# Usage: _needle_event_pulse_scan_completed [issues_found=...] [beads_created=...] [key=value ...]
_needle_event_pulse_scan_completed() {
    _needle_telemetry_emit "pulse.scan_completed" "info" "$@"
}

# Emit pulse.issue_detected event
# Usage: _needle_event_pulse_issue_detected [category=...] [severity=...] [title=...] [key=value ...]
_needle_event_pulse_issue_detected() {
    _needle_telemetry_emit "pulse.issue_detected" "info" "$@"
}

# Emit pulse.detector_started event
# Usage: _needle_event_pulse_detector_started <detector_name> [key=value ...]
_needle_event_pulse_detector_started() {
    local detector_name="$1"
    shift
    _needle_telemetry_emit "pulse.detector_started" "info" "detector=$detector_name" "$@"
}

# Emit pulse.detector_completed event
# Usage: _needle_event_pulse_detector_completed <detector_name> [issues_found=...] [key=value ...]
_needle_event_pulse_detector_completed() {
    local detector_name="$1"
    shift
    _needle_telemetry_emit "pulse.detector_completed" "info" "detector=$detector_name" "$@"
}

# ============================================================================
# Mitosis Events (Bead Decomposition)
# ============================================================================

# Emit bead.mitosis.check event
# Usage: _needle_event_bead_mitosis_check <bead_id> [key=value ...]
_needle_event_bead_mitosis_check() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.mitosis.check" "info" "bead_id=$bead_id" "$@"
}

# Emit bead.mitosis.started event
# Usage: _needle_event_bead_mitosis_started <parent_id> [children_count=...] [key=value ...]
_needle_event_bead_mitosis_started() {
    local parent_id="$1"
    shift
    _needle_telemetry_emit "bead.mitosis.started" "info" "parent_id=$parent_id" "$@"
}

# Emit bead.mitosis.child_created event
# Usage: _needle_event_bead_mitosis_child_created <parent_id> <child_id> [title=...] [key=value ...]
_needle_event_bead_mitosis_child_created() {
    local parent_id="$1"
    local child_id="$2"
    shift 2
    _needle_telemetry_emit "bead.mitosis.child_created" "info" "parent_id=$parent_id" "child_id=$child_id" "$@"
}

# Emit bead.mitosis.complete event
# Usage: _needle_event_bead_mitosis_complete <parent_id> [children_count=...] [children=...] [key=value ...]
_needle_event_bead_mitosis_complete() {
    local parent_id="$1"
    shift
    _needle_telemetry_emit "bead.mitosis.complete" "info" "parent_id=$parent_id" "$@"
}

# Emit bead.mitosis.failed event
# Usage: _needle_event_bead_mitosis_failed <parent_id> [reason=...] [key=value ...]
_needle_event_bead_mitosis_failed() {
    local parent_id="$1"
    shift
    _needle_telemetry_emit "bead.mitosis.failed" "error" "parent_id=$parent_id" "$@"
}

# Emit bead.mitosis.skipped event
# Usage: _needle_event_bead_mitosis_skipped <bead_id> [reason=...] [key=value ...]
_needle_event_bead_mitosis_skipped() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "bead.mitosis.skipped" "info" "bead_id=$bead_id" "$@"
}

# ============================================================================
# Error Events
# ============================================================================

# Emit error.claim_failed event
# Usage: _needle_event_error_claim_failed <bead_id> [reason=...] [key=value ...]
_needle_event_error_claim_failed() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "error.claim_failed" "error" "bead_id=$bead_id" "$@"
}

# Emit error.agent_crash event
# Usage: _needle_event_error_agent_crash [agent=...] [error=...] [key=value ...]
_needle_event_error_agent_crash() {
    _needle_telemetry_emit "error.agent_crash" "error" "pid=$$" "$@"
}

# Emit error.timeout event
# Usage: _needle_event_error_timeout <operation> [duration_seconds=...] [key=value ...]
_needle_event_error_timeout() {
    local operation="$1"
    shift
    _needle_telemetry_emit "error.timeout" "error" "operation=$operation" "$@"
}

# Usage: _needle_event_error_release_failed <bead_id> [reason=...] [key=value ...]
_needle_event_error_release_failed() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "error.release_failed" "error" "bead_id=$bead_id" "$@"
}

# ============================================================================
# Effort Events (Cost Tracking)
# ============================================================================

# Emit effort.recorded event
# Usage: _needle_event_effort_recorded <bead_id> [cost=...] [agent=...] [key=value ...]
_needle_event_effort_recorded() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "effort.recorded" "info" "bead_id=$bead_id" "$@"
}

# ============================================================================
# Budget Events (Budget Enforcement)
# ============================================================================

# Emit budget.warning event
# Usage: _needle_event_budget_warning [daily_spend_usd=...] [daily_limit_usd=...] [threshold=...] [key=value ...]
_needle_event_budget_warning() {
    _needle_telemetry_emit "budget.warning" "warn" "$@"
}

# Emit budget.exceeded event
# Usage: _needle_event_budget_exceeded [daily_spend_usd=...] [daily_limit_usd=...] [key=value ...]
_needle_event_budget_exceeded() {
    _needle_telemetry_emit "budget.exceeded" "error" "$@"
}

# Emit budget.per_bead_exceeded event
# Usage: _needle_event_budget_per_bead_exceeded [bead_cost_usd=...] [bead_limit_usd=...] [bead_id=...] [key=value ...]
_needle_event_budget_per_bead_exceeded() {
    _needle_telemetry_emit "budget.per_bead_exceeded" "error" "$@"
}

# ============================================================================
# File Lock Events
# ============================================================================

# Emit file.checkout event
# Usage: _needle_event_file_checkout <bead_id> [path=...] [status=...] [key=value ...]
_needle_event_file_checkout() {
    local bead_id="$1"
    shift
    _needle_telemetry_emit "file.checkout" "info" "bead_id=$bead_id" "$@"
}

# Emit file.conflict event
# Usage: _needle_event_file_conflict [bead=...] [path=...] [blocked_by=...] [key=value ...]
_needle_event_file_conflict() {
    _needle_telemetry_emit "file.conflict" "warn" "$@"
}

# Emit file.release event
# Usage: _needle_event_file_release [bead=...] [path=...] [held_for_ms=...] [key=value ...]
_needle_event_file_release() {
    _needle_telemetry_emit "file.release" "info" "$@"
}

# Emit file.stale event
# Usage: _needle_event_file_stale [bead=...] [path=...] [age_s=...] [action=...] [key=value ...]
_needle_event_file_stale() {
    _needle_telemetry_emit "file.stale" "warn" "$@"
}

# Emit file.priority_bump event (high-priority bead waiting for lock)
# Usage: _needle_event_file_priority_bump [path=...] [waiting_bead=...] [waiting_priority=...] [holder_bead=...] [holder_priority=...] [key=value ...]
_needle_event_file_priority_bump() {
    _needle_telemetry_emit "lock.priority_bump" "warn" "$@"
}

# Emit file.priority_bump_received event (worker received bump signal)
# Usage: _needle_event_file_priority_bump_received [waiting_bead=...] [waiting_priority=...] [path=...] [key=value ...]
_needle_event_file_priority_bump_received() {
    _needle_telemetry_emit "lock.priority_bump_received" "warn" "$@"
}

# Emit lock.expired event (lock lease expired due to missing heartbeat)
# Usage: _needle_event_lock_expired [bead=...] [path=...] [worker=...] [age_s=...] [key=value ...]
_needle_event_lock_expired() {
    _needle_telemetry_emit "lock.expired" "warn" "$@"
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
mend.orphan_released
mend.heartbeat_cleaned
mend.logs_pruned
mend.completed
unravel.alternatives_created
unravel.alternative_created
unravel.analysis_started
unravel.analysis_completed
weave.bead_created
weave.analysis_started
weave.analysis_completed
pulse.bead_created
pulse.scan_started
pulse.scan_completed
pulse.issue_detected
pulse.detector_started
pulse.detector_completed
bead.mitosis.check
bead.mitosis.started
bead.mitosis.child_created
bead.mitosis.complete
bead.mitosis.failed
bead.mitosis.skipped
error.claim_failed
error.agent_crash
error.timeout
effort.recorded
budget.warning
budget.exceeded
budget.per_bead_exceeded
file.checkout
file.conflict
file.release
file.stale
lock.expired
EOF
}

# Check if an event type is valid
# Usage: _needle_telemetry_valid_event <event_type>
# Returns: 0 if valid, 1 if invalid
_needle_telemetry_valid_event() {
    local event_type="$1"
    _needle_telemetry_event_types | grep -qx "$event_type"
}
