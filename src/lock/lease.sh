#!/usr/bin/env bash
# NEEDLE Lock Lease Management
# Handles lock lease renewal and expiration for heartbeat-based lock validity
#
# Locks require periodic heartbeat to remain valid. When a worker's heartbeat
# stops, all locks held by that worker expire after the lease duration plus
# grace period. This prevents stale locks from blocking other workers indefinitely.
#
# Config:
#   file_locks.lease.duration: How long a lock remains valid without renewal (default: 60s)
#   file_locks.lease.renewal_interval: How often to refresh lock timestamps (default: 15s)
#   file_locks.lease.grace_period: Extra time before expired locks are released (default: 10s)

# Guard against double-sourcing
[[ -n "${_NEEDLE_LEASE_LOADED:-}" ]] && return 0
_NEEDLE_LEASE_LOADED=1

# ============================================================================
# Dependency Checks (Fallbacks if parent modules not loaded)
# ============================================================================

# Ensure logging functions are available
if ! declare -f _needle_info &>/dev/null; then
    _needle_info() { echo "[INFO] $*" >&2; }
    _needle_warn() { echo "[WARN] $*" >&2; }
    _needle_error() { echo "[ERROR] $*" >&2; }
    _needle_debug() { [[ "${NEEDLE_VERBOSE:-}" == "true" ]] && echo "[DEBUG] $*" >&2; }
fi

# Ensure jq check function is available
if ! declare -f _needle_command_exists &>/dev/null; then
    _needle_command_exists() { command -v "$1" &>/dev/null; }
fi

# Ensure telemetry emit function is available
if ! declare -f _needle_telemetry_emit &>/dev/null; then
    _needle_telemetry_emit() {
        local event_type="$1"
        shift
        # Silently ignore if telemetry not available
        return 0
    }
fi

# Ensure lock.expired event helper is available
if ! declare -f _needle_event_lock_expired &>/dev/null; then
    _needle_event_lock_expired() {
        # Fallback to direct telemetry emit
        _needle_telemetry_emit "lock.expired" "warn" "$@"
    }
fi

# Ensure config get function is available
if ! declare -f _needle_config_get &>/dev/null; then
    _needle_config_get() {
        local key="$1"
        local default="${2:-}"
        echo "$default"
    }
fi

# ============================================================================
# Config Helpers
# ============================================================================

# Get lease duration in seconds (default: 60)
# Usage: _needle_lease_get_duration
# Returns: Duration in seconds
_needle_lease_get_duration() {
    local duration
    duration=$(_needle_config_get "file_locks.lease.duration" "60s" 2>/dev/null || echo "60s")
    # Remove 's' suffix if present and convert to number
    duration="${duration%s}"
    echo "${duration:-60}"
}

# Get lease renewal interval in seconds (default: 15)
# Usage: _needle_lease_get_renewal_interval
# Returns: Interval in seconds
_needle_lease_get_renewal_interval() {
    local interval
    interval=$(_needle_config_get "file_locks.lease.renewal_interval" "15s" 2>/dev/null || echo "15s")
    # Remove 's' suffix if present and convert to number
    interval="${interval%s}"
    echo "${interval:-15}"
}

# Get lease grace period in seconds (default: 10)
# Usage: _needle_lease_get_grace_period
# Returns: Grace period in seconds
_needle_lease_get_grace_period() {
    local grace
    grace=$(_needle_config_get "file_locks.lease.grace_period" "10s" 2>/dev/null || echo "10s")
    # Remove 's' suffix if present and convert to number
    grace="${grace%s}"
    echo "${grace:-10}"
}

# ============================================================================
# Lease Renewal Functions
# ============================================================================

# Renew lock leases for all locks held by the current bead
# Updates the timestamp in each lock file to extend the lease
# Usage: renew_lock_leases [bead_id]
# Environment: Uses NEEDLE_BEAD_ID if arg not provided
# Returns: Number of locks renewed
renew_lock_leases() {
    local bead_id="${1:-${NEEDLE_BEAD_ID:-}}"
    local renewed_count=0

    if [[ -z "$bead_id" ]]; then
        _needle_debug "renew_lock_leases: no bead_id provided, skipping"
        echo "0"
        return 0
    fi

    # Find all lock files for this bead
    local lock_pattern="${NEEDLE_LOCK_DIR}/$(basename "$bead_id")-*"
    local lock_files
    lock_files=$(ls $lock_pattern 2>/dev/null || true)

    if [[ -z "$lock_files" ]]; then
        _needle_debug "renew_lock_leases: no locks found for bead: $bead_id"
        return 0
    fi

    local now
    now=$(date +%s)

    for lock_file in $lock_files; do
        [[ -f "$lock_file" ]] || continue

        # Read current lock info
        local lock_info filepath worker_id workspace
        lock_info=$(_needle_lock_read_info "$lock_file")

        if _needle_command_exists jq; then
            filepath=$(echo "$lock_info" | jq -r '.path // "unknown"')
            worker_id=$(echo "$lock_info" | jq -r '.worker // "unknown"')
            workspace=$(echo "$lock_info" | jq -r '.workspace // "unknown"')
        else
            filepath=$(echo "$lock_info" | grep -o '"path":"[^"]*"' | cut -d'"' -f4 || echo "unknown")
            worker_id=$(echo "$lock_info" | grep -o '"worker":"[^"]*"' | cut -d'"' -f4 || echo "unknown")
            workspace=$(echo "$lock_info" | grep -o '"workspace":"[^"]*"' | cut -d'"' -f4 || echo "unknown")
        fi

        # Update the lock file with new timestamp
        if _needle_command_exists jq; then
            jq -n \
                --arg bead "$bead_id" \
                --arg worker "$worker_id" \
                --arg path "$filepath" \
                --arg type "write" \
                --argjson ts "$now" \
                --arg workspace "$workspace" \
                '{bead: $bead, worker: $worker, path: $path, type: $type, ts: $ts, workspace: $workspace}' \
                > "$lock_file"
        else
            # Fallback: manual JSON construction
            cat > "$lock_file" << EOF
{"bead":"$(_needle_json_escape "$bead_id")","worker":"$(_needle_json_escape "$worker_id")","path":"$(_needle_json_escape "$filepath")","type":"write","ts":$now,"workspace":"$(_needle_json_escape "$workspace")"}
EOF
        fi

        renewed_count=$((renewed_count + 1))
        _needle_debug "Renewed lock lease: $filepath (bead: $bead_id)"
    done

    if [[ $renewed_count -gt 0 ]]; then
        _needle_debug "Renewed $renewed_count lock lease(s) for bead: $bead_id"
    fi

    echo "$renewed_count"
    return 0
}

# Helper: Read lock info from lock file (from checkout.sh)
# Usage: _needle_lock_read_info <lock_file>
# Returns: JSON object with lock info
_needle_lock_read_info() {
    local lock_file="$1"

    if [[ ! -f "$lock_file" ]]; then
        echo "{}"
        return 1
    fi

    cat "$lock_file" 2>/dev/null || echo "{}"
}

# Helper: JSON escape function
if ! declare -f _needle_json_escape &>/dev/null; then
    _needle_json_escape() {
        local str="$1"
        str="${str//\\/\\\\}"
        str="${str//\"/\\\"}"
        str="${str//$'\n'/\\n}"
        str="${str//$'\r'/\\r}"
        str="${str//$'\t'/\\t}"
        printf '%s' "$str"
    }
fi

# ============================================================================
# Lease Expiration Functions
# ============================================================================

# Expire stale leases for locks held by dead workers
# Should be called periodically (e.g., from mend strand or watchdog)
# Usage: expire_stale_leases
# Returns: Number of locks expired
expire_stale_leases() {
    local expired_count=0
    local now
    now=$(date +%s)

    # Get lease configuration
    local lease_duration grace_period max_age
    lease_duration=$(_needle_lease_get_duration)
    grace_period=$(_needle_lease_get_grace_period)
    max_age=$((lease_duration + grace_period))

    _needle_debug "expire_stale_leases: checking for locks older than ${max_age}s"

    # Check all lock files
    if [[ ! -d "$NEEDLE_LOCK_DIR" ]]; then
        return 0
    fi

    local lock_files
    lock_files=$(ls "${NEEDLE_LOCK_DIR}"/*-* 2>/dev/null || true)

    for lock_file in $lock_files; do
        [[ -f "$lock_file" ]] || continue

        # Read lock info to get timestamp
        local lock_info ts bead_id filepath worker_id
        lock_info=$(_needle_lock_read_info "$lock_file")

        if _needle_command_exists jq; then
            ts=$(echo "$lock_info" | jq -r '.ts // 0')
            bead_id=$(echo "$lock_info" | jq -r '.bead // "unknown"')
            filepath=$(echo "$lock_info" | jq -r '.path // "unknown"')
            worker_id=$(echo "$lock_info" | jq -r '.worker // "unknown"')
        else
            ts=$(echo "$lock_info" | grep -o '"ts":[0-9]*' | cut -d: -f2 || echo "0")
            bead_id=$(echo "$lock_info" | grep -o '"bead":"[^"]*"' | cut -d'"' -f4 || echo "unknown")
            filepath=$(echo "$lock_info" | grep -o '"path":"[^"]*"' | cut -d'"' -f4 || echo "unknown")
            worker_id=$(echo "$lock_info" | grep -o '"worker":"[^"]*"' | cut -d'"' -f4 || echo "unknown")
        fi

        # Calculate lock age
        local lock_age
        lock_age=$((now - ts))

        # Check if lock is expired (older than max_age)
        if [[ $lock_age -gt $max_age ]]; then
            # Verify worker is actually dead (no heartbeat)
            if _needle_lease_worker_alive "$worker_id" "$bead_id"; then
                # Worker still has active heartbeat, don't expire
                _needle_debug "Skipping lock for alive worker: $worker_id (bead: $bead_id, path: $filepath)"
                continue
            fi

            # Lock is stale and worker is dead - expire it
            local age_s=$((lock_age))
            _needle_warn "Expiring stale lock: $filepath (held by bead $bead_id, age: ${age_s}s)"

            # Emit expiration telemetry event
            _needle_event_lock_expired \
                "bead=$bead_id" \
                "path=$filepath" \
                "worker=$worker_id" \
                "age_s=$age_s"

            # Remove the stale lock file
            if rm -f "$lock_file" 2>/dev/null; then
                expired_count=$((expired_count + 1))
                _needle_debug "Expired lock: $lock_file"
            else
                _needle_error "Failed to expire lock: $lock_file"
            fi
        fi
    done

    if [[ $expired_count -gt 0 ]]; then
        _needle_info "Expired $expired_count stale lock lease(s)"
    fi

    echo "$expired_count"
    return 0
}

# Check if a worker has an active heartbeat for the given bead
# Usage: _needle_lease_worker_alive <worker_id> [bead_id]
# Returns: 0 if worker is alive (has heartbeat), 1 if dead
_needle_lease_worker_alive() {
    local worker_id="$1"
    local bead_id="${2:-}"

    if [[ -z "$worker_id" || "$worker_id" == "unknown" ]]; then
        _needle_debug "worker_alive: no worker ID, assuming dead"
        return 1
    fi

    # Check for heartbeat file
    local heartbeat_file="${NEEDLE_STATE_DIR:-$NEEDLE_HOME/.needle-state}/heartbeats/${worker_id}.json"

    if [[ -z "$NEEDLE_STATE_DIR" ]]; then
        heartbeat_file="$NEEDLE_HOME/.needle-state/heartbeats/${worker_id}.json"
    fi

    if [[ ! -f "$heartbeat_file" ]]; then
        _needle_debug "worker_alive: no heartbeat file for $worker_id"
        return 1
    fi

    # Read heartbeat file
    local heartbeat_data last_heartbeat_ts heartbeat_max_age
    heartbeat_data=$(cat "$heartbeat_file" 2>/dev/null || echo "{}")

    if _needle_command_exists jq; then
        last_heartbeat_ts=$(echo "$heartbeat_data" | jq -r '.last_heartbeat // empty')
    else
        last_heartbeat_ts=$(echo "$heartbeat_data" | grep -o '"last_heartbeat":"[^"]*"' | cut -d'"' -f4 || echo "")
    fi

    if [[ -z "$last_heartbeat_ts" ]]; then
        _needle_debug "worker_alive: no last_heartbeat timestamp for $worker_id"
        return 1
    fi

    # Convert ISO8601 timestamp to unix time
    local last_heartbeat_unix
    if date -d "$last_heartbeat_ts" +%s &>/dev/null; then
        last_heartbeat_unix=$(date -d "$last_heartbeat_ts" +%s)
    elif date -f "$last_heartbeat_ts" +%s &>/dev/null; then
        # macOS compatibility
        last_heartbeat_unix=$(date -f "$last_heartbeat_ts" +%s)
    else
        # Parse ISO8601 manually as fallback
        last_heartbeat_unix=$(date -u -d "${last_heartbeat_ts/T/ }" +%s 2>/dev/null || echo "0")
    fi

    if [[ -z "$last_heartbeat_unix" || "$last_heartbeat_unix" == "0" ]]; then
        _needle_debug "worker_alive: failed to parse heartbeat timestamp for $worker_id"
        return 1
    fi

    # Get heartbeat max age from config (default: 120s)
    heartbeat_max_age=$(_needle_config_get "watchdog.heartbeat_timeout" "120" 2>/dev/null || echo "120")

    # Check if heartbeat is recent enough
    local now heartbeat_age
    now=$(date +%s)
    heartbeat_age=$((now - last_heartbeat_unix))

    if [[ $heartbeat_age -lt $heartbeat_max_age ]]; then
        # If bead_id provided, also verify the heartbeat is for this bead
        if [[ -n "$bead_id" ]]; then
            local current_bead
            if _needle_command_exists jq; then
                current_bead=$(echo "$heartbeat_data" | jq -r '.current_bead // empty')
            else
                current_bead=$(echo "$heartbeat_data" | grep -o '"current_bead":"[^"]*"' | cut -d'"' -f4 || echo "")
            fi

            # Worker is alive if heartbeat shows it's working on this bead
            if [[ "$current_bead" == "$bead_id" ]]; then
                _needle_debug "worker_alive: $worker_id is alive and working on $bead_id"
                return 0
            fi

            # Worker is alive but not working on this bead - check status
            local status
            if _needle_command_exists jq; then
                status=$(echo "$heartbeat_data" | jq -r '.status // "idle"')
            else
                status=$(echo "$heartbeat_data" | grep -o '"status":"[^"]*"' | cut -d'"' -f4 || echo "idle")
            fi

            # Worker is alive if idle (not working on anything)
            if [[ "$status" == "idle" ]]; then
                _needle_debug "worker_alive: $worker_id is alive and idle"
                return 0
            fi

            # Worker is alive but working on a different bead
            _needle_debug "worker_alive: $worker_id is alive but working on $current_bead"
            return 1
        fi

        _needle_debug "worker_alive: $worker_id is alive (heartbeat age: ${heartbeat_age}s)"
        return 0
    fi

    _needle_debug "worker_alive: $worker_id heartbeat is stale (age: ${heartbeat_age}s, max: ${heartbeat_max_age}s)"
    return 1
}

# ============================================================================
# Lease Status Query Functions
# ============================================================================

# Get all locks that are approaching expiration
# Useful for monitoring and diagnostics
# Usage: get_expiring_locks [warning_threshold_seconds]
# Returns: List of locks approaching expiration (one per line)
get_expiring_locks() {
    local warning_threshold="${1:-30}"  # Default: 30 seconds until expiration
    local now
    now=$(date +%s)

    local lease_duration
    lease_duration=$(_needle_lease_get_duration)

    if [[ ! -d "$NEEDLE_LOCK_DIR" ]]; then
        return 0
    fi

    local lock_files
    lock_files=$(ls "${NEEDLE_LOCK_DIR}"/*-* 2>/dev/null || true)

    for lock_file in $lock_files; do
        [[ -f "$lock_file" ]] || continue

        local lock_info ts bead_id filepath
        lock_info=$(_needle_lock_read_info "$lock_file")

        if _needle_command_exists jq; then
            ts=$(echo "$lock_info" | jq -r '.ts // 0')
            bead_id=$(echo "$lock_info" | jq -r '.bead // "unknown"')
            filepath=$(echo "$lock_info" | jq -r '.path // "unknown"')
        else
            ts=$(echo "$lock_info" | grep -o '"ts":[0-9]*' | cut -d: -f2 || echo "0")
            bead_id=$(echo "$lock_info" | grep -o '"bead":"[^"]*"' | cut -d'"' -f4 || echo "unknown")
            filepath=$(echo "$lock_info" | grep -o '"path":"[^"]*"' | cut -d'"' -f4 || echo "unknown")
        fi

        local lock_age time_until_expiry
        lock_age=$((now - ts))
        time_until_expiry=$((lease_duration - lock_age))

        if [[ $time_until_expiry -le $warning_threshold && $time_until_expiry -gt 0 ]]; then
            echo "$bead_id|$filepath|$time_until_expiry"
        fi
    done
}
