#!/usr/bin/env bash
# NEEDLE File Checkout System
# Manages file collision avoidance using /dev/shm locks
#
# Design Principles:
# 1. No blocking - Workers never wait for file locks; conflicts become dependencies
# 2. Self-healing - Closing a bead releases all its file claims automatically
# 3. Cross-workspace - All NEEDLE workers share the same lock namespace
# 4. Volatile - Locks live in /dev/shm (RAM), no stale locks after reboot
#
# Lock Structure:
#   /dev/shm/needle/{bead-id}-{path-uuid}
#   Where path-uuid is the first 8 characters of MD5 hash of the absolute file path
#
# Lock file contents (JSON):
#   {
#     "bead": "nd-2ov",
#     "worker": "claude-code-glm-5-alpha",
#     "path": "/home/coder/NEEDLE/src/cli/run.sh",
#     "type": "write",
#     "ts": 1709337600,
#     "workspace": "/home/coder/NEEDLE"
#   }

# ============================================================================
# Lock Directory Configuration
# ============================================================================

NEEDLE_LOCK_DIR="${NEEDLE_LOCK_DIR:-/dev/shm/needle}"

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

# Ensure JSON escape function is available
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

# Ensure telemetry emit function is available
if ! declare -f _needle_telemetry_emit &>/dev/null; then
    _needle_telemetry_emit() {
        local event_type="$1"
        shift
        # Silently ignore if telemetry not available
        return 0
    }
fi

# ============================================================================
# Utility Functions
# ============================================================================

# Generate path UUID from file path (first 8 chars of MD5)
# Usage: _needle_lock_path_uuid <filepath>
# Returns: 8-character hex string
_needle_lock_path_uuid() {
    local filepath="$1"

    if [[ -z "$filepath" ]]; then
        echo "00000000"
        return 1
    fi

    # Use md5sum to generate hash, take first 8 characters
    echo -n "$filepath" | md5sum | cut -c1-8
}

# Get the lock file path for a specific bead and file
# Usage: _needle_lock_file_path <bead_id> <filepath>
# Returns: Full path to lock file
_needle_lock_file_path() {
    local bead_id="$1"
    local filepath="$2"
    local path_uuid

    path_uuid=$(_needle_lock_path_uuid "$filepath")
    echo "${NEEDLE_LOCK_DIR}/${bead_id}-${path_uuid}"
}

# Ensure lock directory exists
# Usage: _needle_lock_ensure_dir
_needle_lock_ensure_dir() {
    if [[ ! -d "$NEEDLE_LOCK_DIR" ]]; then
        mkdir -p "$NEEDLE_LOCK_DIR" 2>/dev/null || {
            _needle_error "Failed to create lock directory: $NEEDLE_LOCK_DIR"
            return 1
        }
        _needle_debug "Created lock directory: $NEEDLE_LOCK_DIR"
    fi
}

# Write lock info JSON to lock file
# Usage: _needle_lock_write_info <lock_file> <bead_id> <worker_id> <filepath> <workspace>
_needle_lock_write_info() {
    local lock_file="$1"
    local bead_id="$2"
    local worker_id="$3"
    local filepath="$4"
    local workspace="$5"
    local ts
    ts=$(date +%s)

    if _needle_command_exists jq; then
        # Use jq for proper JSON generation
        jq -n \
            --arg bead "$bead_id" \
            --arg worker "$worker_id" \
            --arg path "$filepath" \
            --arg type "write" \
            --argjson ts "$ts" \
            --arg workspace "$workspace" \
            '{bead: $bead, worker: $worker, path: $path, type: $type, ts: $ts, workspace: $workspace}' \
            > "$lock_file"
    else
        # Fallback: manual JSON construction
        cat > "$lock_file" << EOF
{"bead":"$(_needle_json_escape "$bead_id")","worker":"$(_needle_json_escape "$worker_id")","path":"$(_needle_json_escape "$filepath")","type":"write","ts":$ts,"workspace":"$(_needle_json_escape "$workspace")"}
EOF
    fi
}

# Read lock info from lock file
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

# Extract bead ID from lock filename
# Usage: _needle_lock_extract_bead_id <lock_file>
# Returns: Bead ID from filename
_needle_lock_extract_bead_id() {
    local lock_file="$1"
    local filename
    filename=$(basename "$lock_file")

    # Extract bead ID (everything before the last hyphen and path-uuid)
    # Format: {bead-id}-{path-uuid}
    echo "$filename" | rev | cut -d'-' -f2- | rev
}

# ============================================================================
# Core Lock API Functions
# ============================================================================

# Attempt to checkout a file for writing
# Usage: checkout_file <filepath> [bead_id] [worker_id]
# Environment: Uses NEEDLE_BEAD_ID and NEEDLE_WORKER if args not provided
# Returns:
#   0 = Lock acquired successfully
#   1 = File locked by another bead (prints blocking bead info to stdout)
# Example:
#   if ! checkout_file "/path/to/file.sh" "nd-2ov" "claude-code-glm-5"; then
#       echo "Blocked by: $(cat)"
#       exit 1
#   fi
checkout_file() {
    local filepath="$1"
    local bead_id="${2:-${NEEDLE_BEAD_ID:-}}"
    local worker_id="${3:-${NEEDLE_WORKER:-${NEEDLE_SESSION:-unknown}}}"
    local workspace="${NEEDLE_WORKSPACE:-$(pwd)}"

    # Validate inputs
    if [[ -z "$filepath" ]]; then
        _needle_error "checkout_file: filepath is required"
        return 1
    fi

    if [[ -z "$bead_id" ]]; then
        _needle_error "checkout_file: bead_id is required (set NEEDLE_BEAD_ID or pass as argument)"
        return 1
    fi

    # Ensure lock directory exists
    _needle_lock_ensure_dir || return 1

    local path_uuid lock_file
    path_uuid=$(_needle_lock_path_uuid "$filepath")
    lock_file=$(_needle_lock_file_path "$bead_id" "$filepath")

    _needle_debug "Checking out file: $filepath (uuid: $path_uuid)"

    # Check if file is already locked by another bead
    # Look for any lock files with this path UUID
    local existing_locks
    existing_locks=$(ls "${NEEDLE_LOCK_DIR}"/*-"${path_uuid}" 2>/dev/null || true)

    if [[ -n "$existing_locks" ]]; then
        # Found existing locks - check if any are from a different bead
        for existing_lock in $existing_locks; do
            local existing_bead
            existing_bead=$(_needle_lock_extract_bead_id "$existing_lock")

            if [[ "$existing_bead" != "$bead_id" ]]; then
                # File is locked by another bead
                local lock_info
                lock_info=$(_needle_lock_read_info "$existing_lock")

                _needle_warn "File conflict: $filepath is locked by bead $existing_bead"

                # Emit conflict telemetry event
                _needle_telemetry_emit "file.conflict" "warn" \
                    "bead=$bead_id" \
                    "path=$filepath" \
                    "blocked_by=$existing_bead"

                # Return blocking bead info to stdout
                echo "$lock_info"
                return 1
            fi
        done
    fi

    # Create the lock file atomically using mkdir if possible, otherwise touch
    # Using set -C (noclobber) for atomic file creation
    if (set -o noclobber; echo "" > "$lock_file" 2>/dev/null); then
        # Successfully created lock file, now write info
        _needle_lock_write_info "$lock_file" "$bead_id" "$worker_id" "$filepath" "$workspace"

        _needle_debug "Lock acquired: $lock_file"

        # Emit checkout telemetry event
        _needle_telemetry_emit "file.checkout" "info" \
            "bead=$bead_id" \
            "path=$filepath" \
            "status=acquired"

        return 0
    else
        # Race condition - another process created the lock
        _needle_warn "Race condition acquiring lock for: $filepath"

        # Re-check who holds the lock
        local existing_bead
        existing_bead=$(_needle_lock_extract_bead_id "$lock_file")

        if [[ "$existing_bead" != "$bead_id" ]]; then
            local lock_info
            lock_info=$(_needle_lock_read_info "$lock_file")

            _needle_telemetry_emit "file.conflict" "warn" \
                "bead=$bead_id" \
                "path=$filepath" \
                "blocked_by=$existing_bead"

            echo "$lock_info"
            return 1
        fi

        # We already have the lock, just update it
        _needle_lock_write_info "$lock_file" "$bead_id" "$worker_id" "$filepath" "$workspace"
        _needle_debug "Lock refreshed: $lock_file"
        return 0
    fi
}

# Release a specific file lock
# Usage: release_file <filepath> [bead_id]
# Environment: Uses NEEDLE_BEAD_ID if arg not provided
# Returns:
#   0 = Lock released or didn't exist
#   1 = Error releasing lock
# Example:
#   release_file "/path/to/file.sh"
release_file() {
    local filepath="$1"
    local bead_id="${2:-${NEEDLE_BEAD_ID:-}}"
    local lock_file
    local held_for_ms=0

    if [[ -z "$filepath" ]]; then
        _needle_error "release_file: filepath is required"
        return 1
    fi

    if [[ -z "$bead_id" ]]; then
        _needle_error "release_file: bead_id is required (set NEEDLE_BEAD_ID or pass as argument)"
        return 1
    fi

    lock_file=$(_needle_lock_file_path "$bead_id" "$filepath")

    if [[ ! -f "$lock_file" ]]; then
        _needle_debug "No lock to release: $filepath"
        return 0
    fi

    # Calculate how long we held the lock
    local lock_info ts now
    lock_info=$(_needle_lock_read_info "$lock_file")
    if _needle_command_exists jq; then
        ts=$(echo "$lock_info" | jq -r '.ts // 0')
    else
        ts=$(echo "$lock_info" | grep -o '"ts":[0-9]*' | cut -d: -f2 || echo 0)
    fi
    now=$(date +%s)
    held_for_ms=$(( (now - ts) * 1000 ))

    # Remove the lock file
    if rm -f "$lock_file" 2>/dev/null; then
        _needle_debug "Lock released: $lock_file (held for ${held_for_ms}ms)"

        # Emit release telemetry event
        _needle_telemetry_emit "file.release" "info" \
            "bead=$bead_id" \
            "path=$filepath" \
            "held_for_ms=$held_for_ms"

        return 0
    else
        _needle_error "Failed to release lock: $lock_file"
        return 1
    fi
}

# Release ALL locks held by a bead
# Called automatically on bead completion or failure
# Usage: release_bead_locks <bead_id>
# Returns: 0 always (best-effort cleanup)
# Example:
#   release_bead_locks "nd-2ov"
release_bead_locks() {
    local bead_id="${1:-${NEEDLE_BEAD_ID:-}}"
    local released_count=0
    local failed_count=0

    if [[ -z "$bead_id" ]]; then
        _needle_debug "release_bead_locks: no bead_id provided, skipping"
        return 0
    fi

    _needle_debug "Releasing all locks for bead: $bead_id"

    # Find all lock files for this bead
    local lock_pattern="${NEEDLE_LOCK_DIR}/${bead_id}-*"
    local lock_files
    lock_files=$(ls $lock_pattern 2>/dev/null || true)

    if [[ -z "$lock_files" ]]; then
        _needle_debug "No locks found for bead: $bead_id"
        return 0
    fi

    for lock_file in $lock_files; do
        [[ -f "$lock_file" ]] || continue

        # Read lock info for telemetry
        local lock_info filepath ts now held_for_ms
        lock_info=$(_needle_lock_read_info "$lock_file")

        if _needle_command_exists jq; then
            filepath=$(echo "$lock_info" | jq -r '.path // "unknown"')
            ts=$(echo "$lock_info" | jq -r '.ts // 0')
        else
            filepath=$(echo "$lock_info" | grep -o '"path":"[^"]*"' | cut -d'"' -f4 || echo "unknown")
            ts=$(echo "$lock_info" | grep -o '"ts":[0-9]*' | cut -d: -f2 || echo 0)
        fi

        now=$(date +%s)
        held_for_ms=$(( (now - ts) * 1000 ))

        # Remove the lock file
        if rm -f "$lock_file" 2>/dev/null; then
            released_count=$((released_count + 1))

            # Emit release telemetry event
            _needle_telemetry_emit "file.release" "info" \
                "bead=$bead_id" \
                "path=$filepath" \
                "held_for_ms=$held_for_ms"
        else
            failed_count=$((failed_count + 1))
            _needle_warn "Failed to release lock: $lock_file"
        fi
    done

    _needle_info "Released $released_count locks for bead $bead_id (failed: $failed_count)"

    return 0
}

# Check if a file is locked (read-only query)
# Usage: check_file <filepath>
# Returns:
#   0 = File is locked (prints lock info JSON to stdout)
#   1 = File is not locked
# Example:
#   if check_file "/path/to/file.sh"; then
#       echo "File is locked by: $(cat | jq -r '.bead')"
#   fi
check_file() {
    local filepath="$1"
    local path_uuid
    local lock_files

    if [[ -z "$filepath" ]]; then
        _needle_error "check_file: filepath is required"
        return 1
    fi

    path_uuid=$(_needle_lock_path_uuid "$filepath")
    lock_files=$(ls "${NEEDLE_LOCK_DIR}"/*-"${path_uuid}" 2>/dev/null || true)

    if [[ -z "$lock_files" ]]; then
        _needle_debug "File not locked: $filepath"
        return 1
    fi

    # Return info about the first lock found
    local lock_file
    lock_file=$(echo "$lock_files" | head -1)
    local lock_info
    lock_info=$(_needle_lock_read_info "$lock_file")

    echo "$lock_info"
    return 0
}

# List all current locks (for debugging)
# Usage: list_locks [bead_id]
# Arguments:
#   bead_id - Optional, filter to specific bead
# Returns: JSON array of lock info objects
# Example:
#   list_locks
#   list_locks "nd-2ov"
list_locks() {
    local filter_bead_id="${1:-}"
    local lock_pattern="${NEEDLE_LOCK_DIR}/*"
    local locks=()

    if [[ -n "$filter_bead_id" ]]; then
        lock_pattern="${NEEDLE_LOCK_DIR}/${filter_bead_id}-*"
    fi

    local lock_files
    lock_files=$(ls $lock_pattern 2>/dev/null || true)

    if [[ -z "$lock_files" ]]; then
        echo "[]"
        return 0
    fi

    if _needle_command_exists jq; then
        # Build JSON array using jq
        local json_array="[]"
        for lock_file in $lock_files; do
            [[ -f "$lock_file" ]] || continue
            local lock_info
            lock_info=$(_needle_lock_read_info "$lock_file")
            json_array=$(echo "$json_array" | jq --argjson info "$lock_info" '. + [$info]')
        done
        echo "$json_array"
    else
        # Fallback: build JSON array manually
        local first=true
        echo -n "["
        for lock_file in $lock_files; do
            [[ -f "$lock_file" ]] || continue
            if [[ "$first" == "true" ]]; then
                first=false
            else
                echo -n ","
            fi
            _needle_lock_read_info "$lock_file"
        done
        echo "]"
    fi
}

# ============================================================================
# Stale Lock Detection
# ============================================================================

# Check and handle stale locks
# A lock is considered stale if it's older than the configured timeout
# Usage: check_stale_locks [action]
# Arguments:
#   action - warn (default), release, or ignore
# Config:
#   file_locks.timeout - Lock age timeout (default: 30m = 1800s)
#   file_locks.stale_action - Action for stale locks (default: warn)
# Returns: Number of stale locks found
check_stale_locks() {
    local action="${1:-}"
    local now ts timeout_s stale_count=0

    # Get timeout from config (default 30 minutes)
    if declare -f get_config &>/dev/null; then
        local timeout_config
        timeout_config=$(get_config "file_locks.timeout" "30m")
        # Parse timeout (supports: 30m, 1h, 3600s, or just seconds)
        case "$timeout_config" in
            *m) timeout_s=$(( ${timeout_config%m} * 60 )) ;;
            *h) timeout_s=$(( ${timeout_config%h} * 3600 )) ;;
            *s) timeout_s=${timeout_config%s} ;;
            *) timeout_s=$timeout_config ;;
        esac
    else
        timeout_s=1800  # 30 minutes default
    fi

    # Get action from config if not provided
    if [[ -z "$action" ]]; then
        if declare -f get_config &>/dev/null; then
            action=$(get_config "file_locks.stale_action" "warn")
        else
            action="warn"
        fi
    fi

    now=$(date +%s)

    _needle_debug "Checking for stale locks (timeout: ${timeout_s}s, action: $action)"

    # Iterate through all locks
    local lock_pattern="${NEEDLE_LOCK_DIR}/*"
    local lock_files
    lock_files=$(ls $lock_pattern 2>/dev/null || true)

    for lock_file in $lock_files; do
        [[ -f "$lock_file" ]] || continue

        local lock_info bead filepath age_s
        lock_info=$(_needle_lock_read_info "$lock_file")

        if _needle_command_exists jq; then
            ts=$(echo "$lock_info" | jq -r '.ts // 0')
            bead=$(echo "$lock_info" | jq -r '.bead // "unknown"')
            filepath=$(echo "$lock_info" | jq -r '.path // "unknown"')
        else
            ts=$(echo "$lock_info" | grep -o '"ts":[0-9]*' | cut -d: -f2 || echo 0)
            bead=$(echo "$lock_info" | grep -o '"bead":"[^"]*"' | cut -d'"' -f4 || echo "unknown")
            filepath=$(echo "$lock_info" | grep -o '"path":"[^"]*"' | cut -d'"' -f4 || echo "unknown")
        fi

        age_s=$(( now - ts ))

        if (( age_s > timeout_s )); then
            stale_count=$((stale_count + 1))

            _needle_warn "Stale lock detected: $filepath (held by $bead for ${age_s}s)"

            # Emit stale lock telemetry
            _needle_telemetry_emit "file.stale" "warn" \
                "bead=$bead" \
                "path=$filepath" \
                "age_s=$age_s" \
                "action=$action"

            case "$action" in
                release)
                    if rm -f "$lock_file" 2>/dev/null; then
                        _needle_info "Released stale lock: $lock_file"
                    else
                        _needle_error "Failed to release stale lock: $lock_file"
                    fi
                    ;;
                warn)
                    # Already warned above
                    ;;
                ignore)
                    _needle_debug "Ignoring stale lock (action=ignore)"
                    ;;
                *)
                    _needle_warn "Unknown stale action: $action"
                    ;;
            esac
        fi
    done

    if (( stale_count > 0 )); then
        _needle_info "Found $stale_count stale lock(s)"
    fi

    return $stale_count
}

# ============================================================================
# Bead Reap Check (for cleanup_stale_locks)
# ============================================================================

# Check if a bead is still active (open or in_progress)
# Usage: _needle_lock_bead_active <bead_id>
# Returns: 0 if active, 1 if closed/deleted
_needle_lock_bead_active() {
    local bead_id="$1"

    # Check if br command exists
    if ! _needle_command_exists br; then
        _needle_debug "br command not found, assuming bead is active"
        return 0
    fi

    # Query bead status
    local bead_json
    bead_json=$(br show "$bead_id" --json 2>/dev/null || echo "[]")

    if _needle_command_exists jq; then
        # Check if status is open or in_progress
        local status
        status=$(echo "$bead_json" | jq -r '.[0].status // "unknown"')

        case "$status" in
            open|in_progress)
                return 0
                ;;
            *)
                return 1
                ;;
        esac
    else
        # Fallback: simple grep
        if echo "$bead_json" | grep -q '"status"[[:space:]]*:[[:space:]]*"[^"]*closed[^"]*"'; then
            return 1
        fi
        return 0
    fi
}

# Cleanup locks for beads that are no longer active
# Usage: cleanup_reaped_locks
# Returns: Number of locks cleaned up
cleanup_reaped_locks() {
    local cleaned_count=0
    local lock_pattern="${NEEDLE_LOCK_DIR}/*"
    local lock_files

    lock_files=$(ls $lock_pattern 2>/dev/null || true)

    if [[ -z "$lock_files" ]]; then
        _needle_debug "No locks to check for reaped beads"
        return 0
    fi

    _needle_debug "Checking for locks belonging to reaped beads..."

    # Track which beads we've already checked
    declare -A checked_beads

    for lock_file in $lock_files; do
        [[ -f "$lock_file" ]] || continue

        local bead_id
        bead_id=$(_needle_lock_extract_bead_id "$lock_file")

        # Skip if we already checked this bead
        if [[ -n "${checked_beads[$bead_id]:-}" ]]; then
            continue
        fi

        # Check if bead is still active
        if ! _needle_lock_bead_active "$bead_id"; then
            _needle_info "Bead $bead_id is no longer active, releasing its locks"

            # Release all locks for this bead
            release_bead_locks "$bead_id"
            cleaned_count=$((cleaned_count + 1))
        fi

        # Mark as checked
        checked_beads[$bead_id]=1
    done

    if (( cleaned_count > 0 )); then
        _needle_info "Cleaned up locks for $cleaned_count reaped bead(s)"
    fi

    return $cleaned_count
}

# ============================================================================
# CLI Integration Functions
# ============================================================================

# CLI: checkout command
# Usage: _needle_cli_checkout <filepath>
_needle_cli_checkout() {
    local filepath="$1"

    if [[ -z "$filepath" ]]; then
        _needle_error "Usage: needle checkout <filepath>"
        return 1
    fi

    # Resolve to absolute path
    if [[ "$filepath" != /* ]]; then
        filepath="$(pwd)/$filepath"
    fi

    local bead_id="${NEEDLE_BEAD_ID:-cli-$$}"
    local worker_id="${NEEDLE_WORKER:-cli}"

    if checkout_file "$filepath" "$bead_id" "$worker_id"; then
        _needle_success "Checked out: $filepath"
        return 0
    else
        local blocking_info
        blocking_info=$(cat)
        local blocking_bead
        if _needle_command_exists jq; then
            blocking_bead=$(echo "$blocking_info" | jq -r '.bead // "unknown"')
        else
            blocking_bead="unknown"
        fi
        _needle_error "File locked by: $blocking_bead"
        return 1
    fi
}

# CLI: release command
# Usage: _needle_cli_release <filepath>
_needle_cli_release() {
    local filepath="$1"

    if [[ -z "$filepath" ]]; then
        _needle_error "Usage: needle release <filepath>"
        return 1
    fi

    # Resolve to absolute path
    if [[ "$filepath" != /* ]]; then
        filepath="$(pwd)/$filepath"
    fi

    local bead_id="${NEEDLE_BEAD_ID:-cli-$$}"

    if release_file "$filepath" "$bead_id"; then
        _needle_success "Released: $filepath"
        return 0
    else
        return 1
    fi
}

# CLI: status command (for file locks)
# Usage: _needle_cli_lock_status [filepath]
_needle_cli_lock_status() {
    local filepath="$1"

    if [[ -n "$filepath" ]]; then
        # Resolve to absolute path
        if [[ "$filepath" != /* ]]; then
            filepath="$(pwd)/$filepath"
        fi

        if check_file "$filepath"; then
            local lock_info
            lock_info=$(cat)
            local bead worker ts age
            if _needle_command_exists jq; then
                bead=$(echo "$lock_info" | jq -r '.bead')
                worker=$(echo "$lock_info" | jq -r '.worker')
                ts=$(echo "$lock_info" | jq -r '.ts')
                age=$(($(date +%s) - ts))
                _needle_info "File locked by: $bead (worker: $worker, age: ${age}s)"
            else
                echo "$lock_info"
            fi
            return 0
        else
            _needle_info "File not locked: $filepath"
            return 1
        fi
    else
        # List all locks
        local locks
        locks=$(list_locks)
        local count
        if _needle_command_exists jq; then
            count=$(echo "$locks" | jq 'length')
        else
            count=$(echo "$locks" | grep -c '{' || echo 0)
        fi

        if (( count == 0 )); then
            _needle_info "No active file locks"
        else
            _needle_header "Active File Locks ($count)"
            if _needle_command_exists jq; then
                echo "$locks" | jq -r '.[] | "  \(.bead): \(.path) (worker: \(.worker))"'
            else
                echo "$locks"
            fi
        fi
        return 0
    fi
}

# ============================================================================
# Hook Integration
# ============================================================================

# Function to be called from post_complete and on_failure hooks
# Automatically releases all locks for the current bead
# Usage: _needle_hook_release_locks
_needle_hook_release_locks() {
    local bead_id="${NEEDLE_BEAD_ID:-}"

    if [[ -z "$bead_id" ]]; then
        _needle_debug "No bead_id set, skipping lock release"
        return 0
    fi

    _needle_info "Releasing all file locks for bead: $bead_id"
    release_bead_locks "$bead_id"
}
