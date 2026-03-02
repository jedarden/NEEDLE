#!/usr/bin/env bash
# NEEDLE Watchdog Monitor Process
# Monitors worker heartbeats and triggers automatic recovery

# ============================================================================
# Watchdog Configuration
# ============================================================================

# Default intervals (can be overridden via config)
NEEDLE_WATCHDOG_INTERVAL="${NEEDLE_WATCHDOG_INTERVAL:-30}"
NEEDLE_WATCHDOG_HEARTBEAT_TIMEOUT="${NEEDLE_WATCHDOG_HEARTBEAT_TIMEOUT:-120}"
NEEDLE_WATCHDOG_BEAD_TIMEOUT="${NEEDLE_WATCHDOG_BEAD_TIMEOUT:-600}"
NEEDLE_WATCHDOG_RECOVERY_ACTION="${NEEDLE_WATCHDOG_RECOVERY_ACTION:-restart}"

# State files
NEEDLE_WATCHDOG_PID_FILE=""
NEEDLE_WATCHDOG_HEARTBEATS_DIR=""

# ============================================================================
# Watchdog Initialization
# ============================================================================

# Initialize watchdog paths and configuration
# Usage: _needle_watchdog_init
_needle_watchdog_init() {
    NEEDLE_WATCHDOG_PID_FILE="$NEEDLE_HOME/$NEEDLE_STATE_DIR/watchdog.pid"
    NEEDLE_WATCHDOG_HEARTBEATS_DIR="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"

    # Ensure directories exist
    local state_dir
    state_dir=$(dirname "$NEEDLE_WATCHDOG_PID_FILE")
    if [[ ! -d "$state_dir" ]]; then
        mkdir -p "$state_dir" || {
            echo "ERROR: Failed to create state directory: $state_dir" >&2
            return 1
        }
    fi

    if [[ ! -d "$NEEDLE_WATCHDOG_HEARTBEATS_DIR" ]]; then
        mkdir -p "$NEEDLE_WATCHDOG_HEARTBEATS_DIR" || {
            echo "ERROR: Failed to create heartbeats directory: $NEEDLE_WATCHDOG_HEARTBEATS_DIR" >&2
            return 1
        }
    fi

    # Load configuration values
    NEEDLE_WATCHDOG_INTERVAL=$(get_config 'watchdog.interval' '30')
    NEEDLE_WATCHDOG_HEARTBEAT_TIMEOUT=$(get_config 'watchdog.heartbeat_timeout' '120')
    NEEDLE_WATCHDOG_BEAD_TIMEOUT=$(get_config 'watchdog.bead_timeout' '600')
    NEEDLE_WATCHDOG_RECOVERY_ACTION=$(get_config 'watchdog.recovery_action' 'restart')

    return 0
}

# ============================================================================
# Watchdog Status Functions
# ============================================================================

# Check if watchdog is already running
# Usage: _needle_watchdog_is_running
# Returns: 0 if running, 1 if not
_needle_watchdog_is_running() {
    _needle_watchdog_init

    if [[ -f "$NEEDLE_WATCHDOG_PID_FILE" ]]; then
        local pid
        pid=$(cat "$NEEDLE_WATCHDOG_PID_FILE" 2>/dev/null)

        # Check if PID is valid numeric
        if [[ -n "$pid" ]] && [[ "$pid" =~ ^[0-9]+$ ]]; then
            # Check if process is running
            if kill -0 "$pid" 2>/dev/null; then
                return 0
            fi
        fi

        # Stale PID file, remove it
        rm -f "$NEEDLE_WATCHDOG_PID_FILE"
    fi

    return 1
}

# Get the watchdog PID
# Usage: _needle_watchdog_pid
# Returns: PID or empty string if not running
_needle_watchdog_pid() {
    if _needle_watchdog_is_running; then
        cat "$NEEDLE_WATCHDOG_PID_FILE" 2>/dev/null
    fi
}

# ============================================================================
# Watchdog Lifecycle
# ============================================================================

# Start the watchdog process (ensure it's running)
# Usage: _needle_ensure_watchdog
# This is called by cli/run.sh after spawning workers
_needle_ensure_watchdog() {
    _needle_watchdog_init

    # Check if watchdog already running
    if _needle_watchdog_is_running; then
        _needle_debug "Watchdog already running (PID: $(_needle_watchdog_pid))"
        return 0
    fi

    _needle_info "Starting watchdog monitor..."

    # Start watchdog in background
    _needle_watchdog_run &

    local pid=$!
    echo "$pid" > "$NEEDLE_WATCHDOG_PID_FILE"
    disown "$pid"

    _needle_debug "Watchdog started (PID: $pid)"
    _needle_success "Watchdog monitor started"
}

# Stop the watchdog process
# Usage: _needle_watchdog_stop
_needle_watchdog_stop() {
    _needle_watchdog_init

    if ! _needle_watchdog_is_running; then
        _needle_debug "Watchdog not running"
        return 0
    fi

    local pid
    pid=$(_needle_watchdog_pid)

    _needle_info "Stopping watchdog (PID: $pid)..."

    # Send SIGTERM for graceful shutdown
    if kill -TERM "$pid" 2>/dev/null; then
        # Wait for graceful shutdown (max 5 seconds)
        local wait_count=0
        while kill -0 "$pid" 2>/dev/null && ((wait_count < 50)); do
            sleep 0.1
            ((wait_count++))
        done

        # Force kill if still running
        if kill -0 "$pid" 2>/dev/null; then
            _needle_warn "Watchdog did not stop gracefully, force killing"
            kill -KILL "$pid" 2>/dev/null
        fi
    fi

    rm -f "$NEEDLE_WATCHDOG_PID_FILE"
    _needle_success "Watchdog stopped"
}

# ============================================================================
# Watchdog Monitor Loop (Main Process)
# ============================================================================

# Main watchdog monitoring loop
# This function runs as a background process
# Usage: _needle_watchdog_run
_needle_watchdog_run() {
    # Initialize (this runs in a subprocess, so we need to set up paths)
    NEEDLE_HOME="${NEEDLE_HOME:-$HOME/.needle}"
    NEEDLE_STATE_DIR="${NEEDLE_STATE_DIR:-state}"
    NEEDLE_LOG_DIR="${NEEDLE_LOG_DIR:-logs}"

    NEEDLE_WATCHDOG_PID_FILE="$NEEDLE_HOME/$NEEDLE_STATE_DIR/watchdog.pid"
    NEEDLE_WATCHDOG_HEARTBEATS_DIR="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"

    # Ensure heartbeats directory exists
    if [[ ! -d "$NEEDLE_WATCHDOG_HEARTBEATS_DIR" ]]; then
        mkdir -p "$NEEDLE_WATCHDOG_HEARTBEATS_DIR" 2>/dev/null || exit 1
    fi

    # Set up signal handlers for graceful shutdown
    trap '_needle_watchdog_shutdown' TERM INT EXIT

    # Log file for watchdog
    local watchdog_log="$NEEDLE_HOME/$NEEDLE_LOG_DIR/watchdog.jsonl"
    local log_dir
    log_dir=$(dirname "$watchdog_log")
    [[ ! -d "$log_dir" ]] && mkdir -p "$log_dir"

    _needle_watchdog_log "$watchdog_log" "watchdog.started" "Watchdog monitor started"

    # Main monitoring loop
    while true; do
        _needle_watchdog_check_heartbeats "$watchdog_log"

        # Check if any workers remain
        if ! _needle_watchdog_has_workers; then
            _needle_watchdog_log "$watchdog_log" "watchdog.no_workers" "No workers remaining, shutting down"
            break
        fi

        sleep "${NEEDLE_WATCHDOG_INTERVAL%s}"
    done

    _needle_watchdog_log "$watchdog_log" "watchdog.stopped" "Watchdog monitor stopped"
    rm -f "$NEEDLE_WATCHDOG_PID_FILE"
}

# Graceful shutdown handler
_needdog_shutdown() {
    # Remove PID file on exit
    [[ -n "$NEEDLE_WATCHDOG_PID_FILE" ]] && rm -f "$NEEDLE_WATCHDOG_PID_FILE"
    exit 0
}

# Fix typo in function name
_needle_watchdog_shutdown() {
    [[ -n "$NEEDLE_WATCHDOG_PID_FILE" ]] && rm -f "$NEEDLE_WATCHDOG_PID_FILE"
    exit 0
}

# ============================================================================
# Heartbeat Checking
# ============================================================================

# Check all heartbeat files for stuck workers
# Usage: _needle_watchdog_check_heartbeats <log_file>
_needle_watchdog_check_heartbeats() {
    local log_file="$1"
    local now
    now=$(date +%s)

    # Iterate over heartbeat files
    for hb_file in "$NEEDLE_WATCHDOG_HEARTBEATS_DIR"/*.json; do
        [[ ! -f "$hb_file" ]] && continue

        _needle_watchdog_check_single_heartbeat "$hb_file" "$now" "$log_file"
    done
}

# Check a single heartbeat file for timeout conditions
# Usage: _needle_watchdog_check_single_heartbeat <hb_file> <now_ts> <log_file>
_needle_watchdog_check_single_heartbeat() {
    local hb_file="$1"
    local now="$2"
    local log_file="$3"

    # Read heartbeat data
    local hb_data
    hb_data=$(cat "$hb_file" 2>/dev/null)
    [[ -z "$hb_data" ]] && return 0

    # Extract fields
    local worker pid last_heartbeat status current_bead bead_started

    if command -v jq &>/dev/null; then
        worker=$(echo "$hb_data" | jq -r '.worker // "unknown"')
        pid=$(echo "$hb_data" | jq -r '.pid // 0')
        last_heartbeat=$(echo "$hb_data" | jq -r '.last_heartbeat // ""')
        status=$(echo "$hb_data" | jq -r '.status // "unknown"')
        current_bead=$(echo "$hb_data" | jq -r '.current_bead // empty')
        bead_started=$(echo "$hb_data" | jq -r '.bead_started // empty')
    else
        # Fallback: simple parsing
        worker=$(echo "$hb_data" | grep -o '"worker":"[^"]*"' | head -1 | cut -d'"' -f4)
        pid=$(echo "$hb_data" | grep -o '"pid":[0-9]*' | head -1 | cut -d':' -f2)
        last_heartbeat=$(echo "$hb_data" | grep -o '"last_heartbeat":"[^"]*"' | head -1 | cut -d'"' -f4)
        status=$(echo "$hb_data" | grep -o '"status":"[^"]*"' | head -1 | cut -d'"' -f4)
        current_bead=$(echo "$hb_data" | grep -o '"current_bead":"[^"]*"' | head -1 | cut -d'"' -f4)
        bead_started=$(echo "$hb_data" | grep -o '"bead_started":"[^"]*"' | head -1 | cut -d'"' -f4)
    fi

    # Skip if we can't parse required fields
    [[ -z "$worker" ]] && return 0
    [[ -z "$last_heartbeat" ]] && return 0

    # Convert last_heartbeat to timestamp
    local last_beat_ts
    last_beat_ts=$(date -d "$last_heartbeat" +%s 2>/dev/null || echo 0)

    # Skip if timestamp conversion failed
    [[ "$last_beat_ts" -eq 0 ]] && return 0

    # Calculate heartbeat age
    local beat_age=$((now - last_beat_ts))

    # Check for heartbeat timeout
    if ((beat_age > NEEDLE_WATCHDOG_HEARTBEAT_TIMEOUT)); then
        _needle_watchdog_log "$log_file" "heartbeat.stuck_detected" \
            "Worker $worker heartbeat timeout (${beat_age}s > ${NEEDLE_WATCHDOG_HEARTBEAT_TIMEOUT}s)" \
            "worker=$worker" "reason=no_heartbeat" "age=$beat_age" "pid=$pid"

        _needle_watchdog_recover_worker "$worker" "$pid" "$current_bead" "no_heartbeat" "$hb_file" "$log_file"
        return 0
    fi

    # Check for bead execution timeout
    if [[ -n "$bead_started" ]] && [[ -n "$current_bead" ]]; then
        local bead_ts
        bead_ts=$(date -d "$bead_started" +%s 2>/dev/null || echo 0)

        if [[ "$bead_ts" -gt 0 ]]; then
            local bead_age=$((now - bead_ts))

            if ((bead_age > NEEDLE_WATCHDOG_BEAD_TIMEOUT)); then
                _needle_watchdog_log "$log_file" "heartbeat.bead_stuck" \
                    "Worker $worker bead timeout (${bead_age}s > ${NEEDLE_WATCHDOG_BEAD_TIMEOUT}s)" \
                    "worker=$worker" "reason=bead_stuck" "bead=$current_bead" "age=$bead_age" "pid=$pid"

                _needle_watchdog_recover_worker "$worker" "$pid" "$current_bead" "bead_stuck" "$hb_file" "$log_file"
                return 0
            fi
        fi
    fi
}

# Check if there are any active workers
# Usage: _needle_watchdog_has_workers
# Returns: 0 if workers exist, 1 if no workers
_needle_watchdog_has_workers() {
    local hb_count
    hb_count=$(find "$NEEDLE_WATCHDOG_HEARTBEATS_DIR" -name "*.json" -type f 2>/dev/null | wc -l)

    [[ "$hb_count" -gt 0 ]]
}

# ============================================================================
# Recovery Logic
# ============================================================================

# Recover a stuck worker
# Usage: _needle_watchdog_recover_worker <worker> <pid> <current_bead> <reason> <hb_file> <log_file>
_needle_watchdog_recover_worker() {
    local worker="$1"
    local pid="$2"
    local current_bead="$3"
    local reason="$4"
    local hb_file="$5"
    local log_file="$6"

    _needle_watchdog_log "$log_file" "recovery.started" \
        "Starting recovery for worker $worker" \
        "worker=$worker" "reason=$reason" "pid=$pid" "bead=$current_bead"

    # Step 1: Release bead back to queue
    if [[ -n "$current_bead" ]]; then
        _needle_watchdog_release_bead "$current_bead" "$worker" "$log_file"
    fi

    # Step 2: Kill stuck process
    if [[ -n "$pid" ]] && [[ "$pid" =~ ^[0-9]+$ ]] && [[ "$pid" -gt 1 ]]; then
        if kill -0 "$pid" 2>/dev/null; then
            _needle_watchdog_log "$log_file" "recovery.killing_process" \
                "Killing stuck process $pid" "pid=$pid"

            kill -9 "$pid" 2>/dev/null || true
        fi
    fi

    # Step 3: Clean up heartbeat file
    if [[ -f "$hb_file" ]]; then
        rm -f "$hb_file"
        _needle_watchdog_log "$log_file" "recovery.heartbeat_cleaned" \
            "Removed heartbeat file" "file=$hb_file"
    fi

    # Step 4: Respawn worker if configured
    if [[ "$NEEDLE_WATCHDOG_RECOVERY_ACTION" == "restart" ]]; then
        _needle_watchdog_log "$log_file" "recovery.respawning" \
            "Scheduling worker respawn" "worker=$worker"
        # Note: Actual respawn would be handled by the runner system
        # The worker will naturally be restarted on the next poll cycle
    fi

    _needle_watchdog_log "$log_file" "recovery.completed" \
        "Recovery completed for worker $worker" "worker=$worker"
}

# Release a bead back to the queue
# Usage: _needle_watchdog_release_bead <bead_id> <worker> <log_file>
_needle_watchdog_release_bead() {
    local bead_id="$1"
    local worker="$2"
    local log_file="$3"

    # Use br CLI to release the bead
    if command -v br &>/dev/null; then
        local release_result
        release_result=$(br update "$bead_id" --release --actor watchdog 2>&1)

        if [[ $? -eq 0 ]]; then
            _needle_watchdog_log "$log_file" "recovery.bead_released" \
                "Released bead $bead_id back to queue" \
                "bead_id=$bead_id" "actor=watchdog"
        else
            _needle_watchdog_log "$log_file" "recovery.bead_release_failed" \
                "Failed to release bead $bead_id: $release_result" \
                "bead_id=$bead_id" "error=$release_result"
        fi
    else
        _needle_watchdog_log "$log_file" "recovery.brad_unavailable" \
            "br CLI not available, cannot release bead" \
            "bead_id=$bead_id"
    fi
}

# ============================================================================
# Logging
# ============================================================================

# Log a watchdog event
# Usage: _needle_watchdog_log <log_file> <event_type> <message> [key=value ...]
_needle_watchdog_log() {
    local log_file="$1"
    local event_type="$2"
    local message="$3"
    shift 3

    local ts
    ts=$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ 2>/dev/null || date -u +%Y-%m-%dT%H:%M:%S.000Z)

    # Build data object from key=value pairs
    local data_pairs=""
    for pair in "$@"; do
        if [[ "$pair" == *=* ]]; then
            local key="${pair%%=*}"
            local value="${pair#*=}"
            # Escape value for JSON
            value="${value//\\/\\\\}"
            value="${value//\"/\\\"}"
            data_pairs+="\"$key\":\"$value\","
        fi
    done
    data_pairs="${data_pairs%,}"

    local json="{"
    json+="\"ts\":\"$ts\""
    json+=",\"event\":\"$event_type\""
    json+=",\"message\":\"$message\""
    json+=",\"data\":{$data_pairs}"
    json+="}"

    # Append to log file
    if [[ -n "$log_file" ]]; then
        local log_dir
        log_dir=$(dirname "$log_file")
        [[ ! -d "$log_dir" ]] && mkdir -p "$log_dir"
        echo "$json" >> "$log_file"
    fi

    # Also print to stdout if verbose
    if [[ "${NEEDLE_VERBOSE:-}" == "true" ]]; then
        echo "[watchdog] $event_type: $message"
    fi
}

# ============================================================================
# Status and Diagnostics
# ============================================================================

# Show watchdog status
# Usage: _needle_watchdog_status
_needle_watchdog_status() {
    _needle_watchdog_init

    _needle_header "Watchdog Status"

    if _needle_watchdog_is_running; then
        _needle_success "Running (PID: $(_needle_watchdog_pid))"
    else
        _needle_warn "Not running"
    fi

    _needle_print ""
    _needle_section "Configuration"
    _needle_table_row "Interval" "${NEEDLE_WATCHDOG_INTERVAL}s"
    _needle_table_row "Heartbeat timeout" "${NEEDLE_WATCHDOG_HEARTBEAT_TIMEOUT}s"
    _needle_table_row "Bead timeout" "${NEEDLE_WATCHDOG_BEAD_TIMEOUT}s"
    _needle_table_row "Recovery action" "$NEEDLE_WATCHDOG_RECOVERY_ACTION"

    # Count active heartbeats
    local hb_count
    hb_count=$(find "$NEEDLE_WATCHDOG_HEARTBEATS_DIR" -name "*.json" -type f 2>/dev/null | wc -l)
    _needle_print ""
    _needle_section "Workers"
    _needle_table_row "Active workers" "$hb_count"
}
