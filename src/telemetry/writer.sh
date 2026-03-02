#!/usr/bin/env bash
# NEEDLE CLI Telemetry Writer Module
# Output to File and Stdout - Write structured JSONL events to log files

# Telemetry writer state variables
NEEDLE_LOG_FILE=""
NEEDLE_LOG_INITIALIZED=""

# Default configuration
NEEDLE_LOG_MAX_SIZE="${NEEDLE_LOG_MAX_SIZE:-10485760}"  # 10MB default
NEEDLE_LOG_MAX_AGE="${NEEDLE_LOG_MAX_AGE:-30}"           # 30 days default
NEEDLE_LOG_MAX_FILES="${NEEDLE_LOG_MAX_FILES:-10}"       # Keep 10 old log files

# Initialize log file for a session
# Creates log directory if missing and prepares log file
# Usage: _needle_init_log <session_id>
# Example: _needle_init_log "session-abc123"
_needle_init_log() {
    local session="${1:-${NEEDLE_SESSION:-default}}"
    local log_dir

    NEEDLE_LOG_FILE="$NEEDLE_HOME/$NEEDLE_LOG_DIR/${session}.jsonl"
    log_dir=$(dirname "$NEEDLE_LOG_FILE")

    # Create log directory if missing
    if [[ ! -d "$log_dir" ]]; then
        mkdir -p "$log_dir" || {
            _needle_error "Failed to create log directory: $log_dir"
            return 1
        }
    fi

    # Create or touch log file
    if ! touch "$NEEDLE_LOG_FILE" 2>/dev/null; then
        _needle_error "Failed to create log file: $NEEDLE_LOG_FILE"
        return 1
    fi

    NEEDLE_LOG_INITIALIZED="true"
    _needle_debug "Log initialized: $NEEDLE_LOG_FILE"

    return 0
}

# Write an event to the log file atomically
# Uses flock for exclusive locking to prevent interleaved writes
# Usage: _needle_write_event <json_string>
# Example: _needle_write_event '{"type":"status","message":"Running"}'
_needle_write_event() {
    local event_json="$1"

    # Ensure log is initialized
    if [[ -z "$NEEDLE_LOG_INITIALIZED" ]] || [[ -z "$NEEDLE_LOG_FILE" ]]; then
        _needle_init_log "${NEEDLE_SESSION:-default}" || return 1
    fi

    # Validate JSON is not empty
    if [[ -z "$event_json" ]]; then
        _needle_warn "Attempted to write empty event, skipping"
        return 1
    fi

    # Atomic append with flock using file descriptor 200
    # The flock ensures exclusive access across multiple processes
    (
        flock -x 200 || {
            _needle_error "Failed to acquire lock on log file"
            return 1
        }

        # Append event with newline
        printf '%s\n' "$event_json" >> "$NEEDLE_LOG_FILE"

        flock -u 200
    ) 200>>"$NEEDLE_LOG_FILE"

    local write_status=$?

    # Mirror to stdout if verbose mode is enabled
    if [[ "$write_status" -eq 0 ]] && [[ "$NEEDLE_VERBOSE" == "true" ]]; then
        printf '%s\n' "$event_json"
    fi

    return $write_status
}

# Write an event with automatic JSON formatting
# Combines json.sh emit functions with writer
# Usage: _needle_write_formatted_event --type <type> [--key value]...
# Example: _needle_write_formatted_event --type status --message "Running" --progress 50
_needle_write_formatted_event() {
    local event_json
    event_json=$(_needle_json_emit "$@")

    if [[ -z "$event_json" ]]; then
        _needle_error "Failed to format event"
        return 1
    fi

    _needle_write_event "$event_json"
}

# Rotate logs when they exceed max size
# Moves current log to timestamped backup and creates new log
# Usage: _needle_rotate_logs [max_size_bytes]
# Example: _needle_rotate_logs 5242880  # 5MB
_needle_rotate_logs() {
    local max_size="${1:-$NEEDLE_LOG_MAX_SIZE}"

    # Ensure log is initialized
    if [[ -z "$NEEDLE_LOG_FILE" ]] || [[ ! -f "$NEEDLE_LOG_FILE" ]]; then
        return 0
    fi

    # Get current log size
    local log_size
    log_size=$(stat -c%s "$NEEDLE_LOG_FILE" 2>/dev/null || echo 0)

    # Check if rotation is needed
    if (( log_size <= max_size )); then
        return 0
    fi

    local timestamp
    timestamp=$(date +%s)
    local rotated_file="${NEEDLE_LOG_FILE}.${timestamp}.old"

    # Rotate the log file
    if mv "$NEEDLE_LOG_FILE" "$rotated_file" 2>/dev/null; then
        _needle_debug "Rotated log: $NEEDLE_LOG_FILE -> $rotated_file"

        # Create new empty log file
        touch "$NEEDLE_LOG_FILE"

        # Clean up old rotated files (keep only NEEDLE_LOG_MAX_FILES)
        _needle_cleanup_old_logs

        return 0
    else
        _needle_error "Failed to rotate log file: $NEEDLE_LOG_FILE"
        return 1
    fi
}

# Clean up old rotated log files
# Keeps only the most recent NEEDLE_LOG_MAX_FILES rotated logs
# Usage: _needle_cleanup_old_logs
_needle_cleanup_old_logs() {
    local log_dir log_base keep_count

    if [[ -z "$NEEDLE_LOG_FILE" ]]; then
        return 0
    fi

    log_dir=$(dirname "$NEEDLE_LOG_FILE")
    log_base=$(basename "$NEEDLE_LOG_FILE")
    keep_count="${NEEDLE_LOG_MAX_FILES:-10}"

    # Find and delete old rotated logs beyond keep_count
    # List rotated files sorted by modification time (oldest first)
    local old_files
    old_files=$(find "$log_dir" -name "${log_base}.*.old" -type f -printf '%T@ %p\n' 2>/dev/null | \
                sort -n | head -n -"$keep_count" | cut -d' ' -f2-)

    if [[ -n "$old_files" ]]; then
        while IFS= read -r file; do
            if [[ -n "$file" ]]; then
                rm -f "$file"
                _needle_debug "Removed old log: $file"
            fi
        done <<< "$old_files"
    fi
}

# Clean up logs older than max age
# Removes rotated logs older than NEEDLE_LOG_MAX_AGE days
# Usage: _needle_clean_old_logs [max_age_days]
# Example: _needle_clean_old_logs 7  # Clean logs older than 7 days
_needle_clean_old_logs() {
    local max_age="${1:-$NEEDLE_LOG_MAX_AGE}"
    local log_dir log_base

    if [[ -z "$NEEDLE_LOG_FILE" ]]; then
        # Use default log directory
        log_dir="$NEEDLE_HOME/$NEEDLE_LOG_DIR"
    else
        log_dir=$(dirname "$NEEDLE_LOG_FILE")
    fi

    if [[ ! -d "$log_dir" ]]; then
        return 0
    fi

    # Find and delete rotated logs older than max_age days
    local deleted_count
    deleted_count=$(find "$log_dir" -name "*.old" -type f -mtime +"$max_age" -delete -print 2>/dev/null | wc -l)

    if [[ "$deleted_count" -gt 0 ]]; then
        _needle_debug "Cleaned up $deleted_count old log file(s)"
    fi
}

# Get current log file path
# Usage: _needle_log_file
_needle_log_file() {
    echo "$NEEDLE_LOG_FILE"
}

# Check if log is initialized
# Usage: _needle_log_is_initialized
_needle_log_is_initialized() {
    [[ -n "$NEEDLE_LOG_INITIALIZED" ]] && [[ -n "$NEEDLE_LOG_FILE" ]] && [[ -f "$NEEDLE_LOG_FILE" ]]
}

# Get current log file size in bytes
# Usage: _needle_log_size
_needle_log_size() {
    if _needle_log_is_initialized; then
        stat -c%s "$NEEDLE_LOG_FILE" 2>/dev/null || echo 0
    else
        echo 0
    fi
}

# Read all events from the current log file
# Usage: _needle_log_read
_needle_log_read() {
    if _needle_log_is_initialized; then
        cat "$NEEDLE_LOG_FILE"
    else
        _needle_error "Log not initialized"
        return 1
    fi
}

# Read the last N events from the log
# Usage: _needle_log_tail [count]
# Example: _needle_log_tail 10
_needle_log_tail() {
    local count="${1:-10}"

    if _needle_log_is_initialized; then
        tail -n "$count" "$NEEDLE_LOG_FILE"
    else
        _needle_error "Log not initialized"
        return 1
    fi
}

# Flush log buffer (no-op for file-based logging, but provides API compatibility)
# Usage: _needle_log_flush
_needle_log_flush() {
    if _needle_log_is_initialized; then
        sync "$NEEDLE_LOG_FILE" 2>/dev/null || true
    fi
}

# Close log file and clean up
# Usage: _needle_log_close
_needle_log_close() {
    if _needle_log_is_initialized; then
        _needle_log_flush
        _needle_debug "Log closed: $NEEDLE_LOG_FILE"
    fi

    NEEDLE_LOG_INITIALIZED=""
    # Don't clear NEEDLE_LOG_FILE - it may be needed for later reference
}

# Convenience function to write a telemetry event
# Combines initialization check, rotation check, and writing
# Usage: _needle_telemetry <event_type> <message> [key=value...]
# Example: _needle_telemetry "progress" "Processing files" "percent=50" "files=10"
_needle_telemetry() {
    local event_type="$1"
    local message="$2"
    shift 2

    # Check for rotation before writing
    _needle_rotate_logs

    # Write formatted event
    _needle_write_formatted_event --type "$event_type" --message "$message" "$@"
}

# Convenience function to write a raw JSON event
# Usage: _needle_telemetry_raw <json_string>
# Example: _needle_telemetry_raw '{"type":"custom","data":{"key":"value"}}'
_needle_telemetry_raw() {
    # Check for rotation before writing
    _needle_rotate_logs

    # Write raw event
    _needle_write_event "$1"
}
