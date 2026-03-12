#!/usr/bin/env bash
# NEEDLE CLI Worker Heartbeat Module
# Heartbeat emission for worker liveness tracking and stuck detection

# Heartbeat state variables
NEEDLE_HEARTBEAT_FILE=""
NEEDLE_HEARTBEAT_STARTED=""

# Initialize heartbeat system for this worker
# Creates heartbeat directory and emits initial heartbeat
# Usage: _needle_heartbeat_init
_needle_heartbeat_init() {
    # Set heartbeat file path based on session ID
    # NEEDLE_STATE_DIR may be a full path (set by _needle_run_worker) or relative.
    # Use it directly if absolute, otherwise prepend NEEDLE_HOME.
    if [[ "$NEEDLE_STATE_DIR" == /* ]]; then
        NEEDLE_HEARTBEAT_FILE="$NEEDLE_STATE_DIR/heartbeats/${NEEDLE_SESSION}.json"
    else
        NEEDLE_HEARTBEAT_FILE="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats/${NEEDLE_SESSION}.json"
    fi
    NEEDLE_HEARTBEAT_STARTED=$(date -u +%Y-%m-%dT%H:%M:%SZ)

    # Ensure heartbeat directory exists
    local heartbeat_dir
    heartbeat_dir=$(dirname "$NEEDLE_HEARTBEAT_FILE")
    if [[ ! -d "$heartbeat_dir" ]]; then
        mkdir -p "$heartbeat_dir" || {
            _needle_error "Failed to create heartbeat directory: $heartbeat_dir"
            return 1
        }
    fi

    # Emit initial heartbeat with "starting" status
    _needle_emit_heartbeat "starting"

    _needle_debug "Heartbeat initialized: $NEEDLE_HEARTBEAT_FILE"
}

# Emit a heartbeat with current worker state
# Usage: _needle_emit_heartbeat <status> [current_bead] [bead_started] [strand] [queue_depth]
#   status: idle, executing, draining, starting
#   current_bead: optional bead ID being processed
#   bead_started: optional ISO8601 timestamp when bead processing started
#   strand: optional strand number
#   queue_depth: optional number of beads claimed by this worker
_needle_emit_heartbeat() {
    local status="${1:-idle}"
    local current_bead="${2:-}"
    local bead_started="${3:-}"
    local strand="${4:-}"
    local queue_depth="${5:-}"

    # Validate status
    case "$status" in
        idle|executing|draining|starting)
            ;;
        *)
            _needle_warn "Invalid heartbeat status: $status, defaulting to idle"
            status="idle"
            ;;
    esac

    # Get current timestamp
    local last_heartbeat
    last_heartbeat=$(date -u +%Y-%m-%dT%H:%M:%SZ)

    # Build heartbeat JSON
    # Using jq if available, otherwise fall back to manual JSON construction
    if _needle_command_exists jq; then
        _needle_emit_heartbeat_jq "$status" "$current_bead" "$bead_started" "$strand" "$last_heartbeat"
    else
        _needle_emit_heartbeat_builtin "$status" "$current_bead" "$bead_started" "$strand" "$last_heartbeat"
    fi

    # Emit telemetry event
    _needle_emit_event "heartbeat.emitted" \
        "Worker heartbeat emitted" \
        "status=$status" \
        "current_bead=${current_bead:-none}"
}

# Emit heartbeat using jq (preferred method)
_needle_emit_heartbeat_jq() {
    local status="$1"
    local current_bead="$2"
    local bead_started="$3"
    local strand="$4"
    local last_heartbeat="$5"
    local queue_depth="${6:-}"

    # Build JSON object with jq, handling null values correctly
    local json
    json=$(jq -nc \
        --arg worker "${NEEDLE_SESSION:-unknown}" \
        --arg pid "$$" \
        --arg started "${NEEDLE_HEARTBEAT_STARTED:-}" \
        --arg last_heartbeat "$last_heartbeat" \
        --arg status "$status" \
        --arg current_bead "$current_bead" \
        --arg bead_started "$bead_started" \
        --arg strand "$strand" \
        --arg workspace "${NEEDLE_WORKSPACE:-}" \
        --arg agent "${NEEDLE_AGENT:-unknown}" \
        --argjson queue_depth "${queue_depth:-0}" \
        '{
            worker: $worker,
            pid: ($pid | tonumber),
            started: (if $started == "" then null else $started end),
            last_heartbeat: $last_heartbeat,
            status: $status,
            current_bead: (if $current_bead == "" then null else $current_bead end),
            bead_started: (if $bead_started == "" then null else $bead_started end),
            strand: (if $strand == "" then null else ($strand | tonumber) end),
            workspace: (if $workspace == "" then null else $workspace end),
            agent: $agent,
            queue_depth: $queue_depth
        }')

    if [[ $? -eq 0 && -n "$json" ]]; then
        echo "$json" > "$NEEDLE_HEARTBEAT_FILE"
    else
        _needle_error "Failed to generate heartbeat JSON with jq"
        _needle_emit_heartbeat_builtin "$status" "$current_bead" "$bead_started" "$strand" "$last_heartbeat"
    fi
}

# Emit heartbeat using built-in JSON construction (fallback)
_needle_emit_heartbeat_builtin() {
    local status="$1"
    local current_bead="$2"
    local bead_started="$3"
    local strand="$4"
    local last_heartbeat="$5"
    local queue_depth="${6:-0}"

    # Escape values for JSON
    local worker_escaped
    worker_escaped=$(_needle_json_escape "${NEEDLE_SESSION:-unknown}")
    local agent_escaped
    agent_escaped=$(_needle_json_escape "${NEEDLE_AGENT:-unknown}")
    local workspace_escaped
    workspace_escaped=$(_needle_json_escape "${NEEDLE_WORKSPACE:-}")

    # Build JSON manually
    local json="{"
    json+="\"worker\":\"$worker_escaped\""
    json+=",\"pid\":$$"
    json+=",\"started\":$(_needle_json_nullable "$NEEDLE_HEARTBEAT_STARTED")"
    json+=",\"last_heartbeat\":\"$last_heartbeat\""
    json+=",\"status\":\"$status\""
    json+=",\"current_bead\":$(_needle_json_nullable "$current_bead")"
    json+=",\"bead_started\":$(_needle_json_nullable "$bead_started")"
    json+=",\"strand\":$(_needle_json_nullable_number "$strand")"
    json+=",\"workspace\":$(_needle_json_nullable "$NEEDLE_WORKSPACE")"
    json+=",\"agent\":\"$agent_escaped\""
    json+=",\"queue_depth\":${queue_depth:-0}"
    json+="}"

    echo "$json" > "$NEEDLE_HEARTBEAT_FILE"
}

# Helper: Return null for empty string, or quoted value
_needle_json_nullable() {
    local value="$1"
    if [[ -z "$value" ]]; then
        echo "null"
    else
        local escaped
        escaped=$(_needle_json_escape "$value")
        echo "\"$escaped\""
    fi
}

# Helper: Return null for empty string, or numeric value
_needle_json_nullable_number() {
    local value="$1"
    if [[ -z "$value" ]]; then
        echo "null"
    else
        echo "$value"
    fi
}

# Signal that worker is starting a bead
# Usage: _needle_heartbeat_start_bead <bead_id> [strand]
_needle_heartbeat_start_bead() {
    local bead_id="$1"
    local strand="${2:-}"

    if [[ -z "$bead_id" ]]; then
        _needle_error "Cannot start bead heartbeat: no bead ID provided"
        return 1
    fi

    local bead_started
    bead_started=$(date -u +%Y-%m-%dT%H:%M:%SZ)

    _needle_emit_heartbeat "executing" "$bead_id" "$bead_started" "$strand"

    _needle_debug "Started bead heartbeat: $bead_id (strand: ${strand:-none})"
}

# Signal that worker finished a bead (return to idle)
# Usage: _needle_heartbeat_end_bead
_needle_heartbeat_end_bead() {
    _needle_emit_heartbeat "idle"

    _needle_debug "Ended bead heartbeat, returned to idle"
}

# Signal that worker is draining (shutting down gracefully)
# Usage: _needle_heartbeat_draining
_needle_heartbeat_draining() {
    _needle_emit_heartbeat "draining"

    _needle_debug "Worker entering draining state"
}

# Clean up heartbeat file (call on worker exit)
# Usage: _needle_heartbeat_cleanup
_needle_heartbeat_cleanup() {
    if [[ -n "$NEEDLE_HEARTBEAT_FILE" && -f "$NEEDLE_HEARTBEAT_FILE" ]]; then
        rm -f "$NEEDLE_HEARTBEAT_FILE"
        _needle_debug "Cleaned up heartbeat file: $NEEDLE_HEARTBEAT_FILE"
    fi
}

# Get the current heartbeat file path
# Usage: _needle_heartbeat_file
_needle_heartbeat_file() {
    echo "$NEEDLE_HEARTBEAT_FILE"
}

# Check if heartbeat system is initialized
# Usage: _needle_heartbeat_is_initialized
_needle_heartbeat_is_initialized() {
    [[ -n "$NEEDLE_HEARTBEAT_FILE" && -f "$NEEDLE_HEARTBEAT_FILE" ]]
}

# Read current heartbeat data (returns JSON)
# Usage: _needle_heartbeat_read
_needle_heartbeat_read() {
    if [[ -f "$NEEDLE_HEARTBEAT_FILE" ]]; then
        cat "$NEEDLE_HEARTBEAT_FILE"
    else
        echo '{"error": "No heartbeat file found"}'
        return 1
    fi
}

# Update heartbeat with periodic keepalive (call from worker loop)
# Usage: _needle_heartbeat_keepalive
_needle_heartbeat_keepalive() {
    # Just refresh the last_heartbeat timestamp without changing status
    if _needle_heartbeat_is_initialized; then
        # Read current state
        local current_data
        current_data=$(_needle_heartbeat_read 2>/dev/null)

        if [[ -n "$current_data" ]] && _needle_command_exists jq; then
            # Extract current values
            local status current_bead bead_started strand
            status=$(echo "$current_data" | jq -r '.status // "idle"')
            current_bead=$(echo "$current_data" | jq -r '.current_bead // empty')
            bead_started=$(echo "$current_data" | jq -r '.bead_started // empty')
            strand=$(echo "$current_data" | jq -r '.strand // empty')

            # Re-emit with same values (refreshes timestamp)
            _needle_emit_heartbeat "$status" "$current_bead" "$bead_started" "$strand"
        else
            # Fallback: just emit idle
            _needle_emit_heartbeat "idle"
        fi
    fi
}
