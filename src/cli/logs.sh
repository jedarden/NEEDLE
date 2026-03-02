#!/usr/bin/env bash
# NEEDLE CLI Logs Subcommand
# View and filter worker logs

_needle_logs_help() {
    _needle_print "Usage: needle logs [WORKER] [OPTIONS]

View or tail worker logs

Displays structured JSONL logs from worker sessions. Can filter
by event type, time range, or bead.

Arguments:
    WORKER              Worker identifier [default: all workers]

Options:
    -f, --follow        Follow log output (like tail -f)
    -n, --lines <N>     Number of lines to show [default: 50]

    --since <TIME>      Show logs since timestamp or duration
                        (e.g., \"2024-01-01\", \"1h\", \"30m\")
    --until <TIME>      Show logs until timestamp

    --event <TYPE>      Filter by event type (e.g., \"bead.completed\")
    --bead <ID>         Filter by bead ID
    --strand <N>        Filter by strand number (1-7)

    --raw               Show raw JSONL without formatting
    -j, --json          Output as JSON array

    -h, --help          Print help information

Event Types:
    worker.*      Worker lifecycle (started, stopped, idle)
    bead.*        Bead processing (claimed, completed, failed)
    strand.*      Strand transitions (started, fallthrough)
    hook.*        Hook execution (started, completed, failed)
    heartbeat.*   Heartbeat events
    error.*       Error events

Examples:
    needle logs                           View recent logs for all workers
    needle logs alpha --follow            Follow logs for worker alpha
    needle logs --lines=100               Show last 100 lines
    needle logs --event=bead.completed    Filter by event type
    needle logs --bead=bd-123             Show logs for specific bead
    needle logs --since=1h                Logs from last hour
    needle logs --raw | jq 'select(.event == \"bead.failed\")'
"
}

_needle_logs() {
    local worker=""
    local follow=false
    local lines=50
    local since=""
    local until=""
    local event_filter=""
    local bead_filter=""
    local strand_filter=""
    local raw=false
    local json_output=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -f|--follow)
                follow=true
                shift
                ;;
            -n|--lines)
                lines="$2"
                shift 2
                ;;
            --since)
                since="$2"
                shift 2
                ;;
            --until)
                until="$2"
                shift 2
                ;;
            --event)
                event_filter="$2"
                shift 2
                ;;
            --bead)
                bead_filter="$2"
                shift 2
                ;;
            --strand)
                strand_filter="$2"
                shift 2
                ;;
            --raw)
                raw=true
                shift
                ;;
            -j|--json)
                json_output=true
                shift
                ;;
            -h|--help)
                _needle_logs_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            -*)
                _needle_error "Unknown option: $1"
                _needle_logs_help
                exit $NEEDLE_EXIT_USAGE
                ;;
            *)
                if [[ -z "$worker" ]]; then
                    worker="$1"
                else
                    _needle_error "Unexpected argument: $1"
                    exit $NEEDLE_EXIT_USAGE
                fi
                shift
                ;;
        esac
    done

    # Get log directory
    local log_dir="$NEEDLE_HOME/$NEEDLE_LOG_DIR"

    # Check if log directory exists
    if [[ ! -d "$log_dir" ]]; then
        _needle_warn "No logs directory found: $log_dir"
        _needle_info "Workers will create logs when they start"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # Determine log files to read
    local log_files=()

    if [[ -n "$worker" ]]; then
        # Match worker identifier in log filenames
        # Patterns: needle-*-$worker.jsonl or *$worker*.jsonl
        shopt -s nullglob
        log_files=("$log_dir"/*"$worker"*.jsonl)
        shopt -u nullglob

        if [[ ${#log_files[@]} -eq 0 ]]; then
            _needle_warn "No log files found for worker: $worker"
            _needle_info "Available workers:"
            shopt -s nullglob
            local all_logs=("$log_dir"/*.jsonl)
            shopt -u nullglob
            if [[ ${#all_logs[@]} -gt 0 ]]; then
                for f in "${all_logs[@]}"; do
                    local basename
                    basename=$(basename "$f" .jsonl)
                    _needle_print "  $basename"
                done
            else
                _needle_print "  (none)"
            fi
            exit $NEEDLE_EXIT_SUCCESS
        fi
    else
        shopt -s nullglob
        log_files=("$log_dir"/*.jsonl)
        shopt -u nullglob

        if [[ ${#log_files[@]} -eq 0 ]]; then
            _needle_info "No log files found"
            _needle_info "Workers will create logs when they start"
            exit $NEEDLE_EXIT_SUCCESS
        fi
    fi

    # Build time filter
    local time_filter=""
    if [[ -n "$since" ]] || [[ -n "$until" ]]; then
        time_filter=$(_needle_build_time_filter "$since" "$until")
    fi

    # Build jq filter
    local jq_filter="."
    local has_filters=false

    if [[ -n "$event_filter" ]]; then
        # Support wildcards like "bead.*" or exact match
        if [[ "$event_filter" == *"*"* ]]; then
            # Convert wildcard to jq starts_with or contains
            local prefix="${event_filter%\*}"
            jq_filter+=" | select(.event | startswith(\"$prefix\"))"
        else
            jq_filter+=" | select(.event == \"$event_filter\")"
        fi
        has_filters=true
    fi

    if [[ -n "$bead_filter" ]]; then
        jq_filter+=" | select(.data.bead_id == \"$bead_filter\" or .bead_id == \"$bead_filter\")"
        has_filters=true
    fi

    if [[ -n "$strand_filter" ]]; then
        jq_filter+=" | select(.data.strand == $strand_filter)"
        has_filters=true
    fi

    if [[ -n "$time_filter" ]]; then
        jq_filter+=" | $time_filter"
        has_filters=true
    fi

    # Check if jq is available
    if ! _needle_command_exists jq; then
        _needle_warn "jq not found - showing raw logs without filtering"
        raw=true
        jq_filter="."
    fi

    if $follow; then
        # Tail with follow mode
        _needle_verbose "Following logs from ${#log_files[@]} file(s)..."

        if $raw; then
            # Raw output
            tail -f "${log_files[@]}" 2>/dev/null
        else
            # Formatted output with follow
            tail -f "${log_files[@]}" 2>/dev/null | while IFS= read -r line; do
                if [[ -n "$line" ]]; then
                    if $has_filters; then
                        # Apply jq filter
                        local filtered
                        filtered=$(echo "$line" | jq -c "$jq_filter" 2>/dev/null)
                        if [[ -n "$filtered" ]] && [[ "$filtered" != "null" ]]; then
                            _needle_format_log_line "$filtered"
                        fi
                    else
                        _needle_format_log_line "$line"
                    fi
                fi
            done
        fi
    else
        # Static read mode
        if $json_output; then
            # Output as JSON array
            if _needle_command_exists jq; then
                if $has_filters; then
                    cat "${log_files[@]}" 2>/dev/null | jq -c "$jq_filter" 2>/dev/null | jq -s '.'
                else
                    cat "${log_files[@]}" 2>/dev/null | jq -s '.'
                fi
            else
                _needle_error "jq is required for --json output"
                exit $NEEDLE_EXIT_RUNTIME
            fi
        elif $raw; then
            # Raw output
            if $has_filters && _needle_command_exists jq; then
                cat "${log_files[@]}" 2>/dev/null | jq -c "$jq_filter" 2>/dev/null | tail -n "$lines"
            else
                cat "${log_files[@]}" 2>/dev/null | tail -n "$lines"
            fi
        else
            # Formatted output
            _needle_verbose "Reading logs from ${#log_files[@]} file(s)..."

            if _needle_command_exists jq; then
                if $has_filters; then
                    cat "${log_files[@]}" 2>/dev/null | \
                        jq -c "$jq_filter" 2>/dev/null | \
                        tail -n "$lines" | \
                        while IFS= read -r line; do
                            if [[ -n "$line" ]] && [[ "$line" != "null" ]]; then
                                _needle_format_log_line "$line"
                            fi
                        done
                else
                    cat "${log_files[@]}" 2>/dev/null | \
                        tail -n "$lines" | \
                        while IFS= read -r line; do
                            if [[ -n "$line" ]]; then
                                _needle_format_log_line "$line"
                            fi
                        done
                fi
            else
                # Fallback without jq
                cat "${log_files[@]}" 2>/dev/null | tail -n "$lines"
            fi
        fi
    fi

    exit $NEEDLE_EXIT_SUCCESS
}

# Build jq time filter from since/until arguments
_needle_build_time_filter() {
    local since="$1"
    local until="$2"
    local filter="."

    # Convert relative times to ISO format for comparison
    # jq can compare ISO 8601 timestamps lexicographically
    if [[ -n "$since" ]]; then
        local since_ts
        since_ts=$(_needle_parse_time_arg "$since")
        if [[ -n "$since_ts" ]]; then
            filter+=" | select(.ts >= \"$since_ts\")"
        fi
    fi

    if [[ -n "$until" ]]; then
        local until_ts
        until_ts=$(_needle_parse_time_arg "$until")
        if [[ -n "$until_ts" ]]; then
            filter+=" | select(.ts <= \"$until_ts\")"
        fi
    fi

    echo "$filter"
}

# Parse time argument to ISO 8601 format
# Supports: "1h", "30m", "2d", "2024-01-01", ISO timestamps
_needle_parse_time_arg() {
    local arg="$1"

    # Check if it's already an ISO timestamp or date
    if [[ "$arg" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2} ]]; then
        # Already in date format, return as-is or with time
        if [[ "$arg" =~ T ]]; then
            echo "$arg"
        else
            echo "${arg}T00:00:00Z"
        fi
        return
    fi

    # Parse relative time (e.g., "1h", "30m", "2d")
    local seconds=0
    if [[ "$arg" =~ ^([0-9]+)([smhd])$ ]]; then
        local num="${BASH_REMATCH[1]}"
        local unit="${BASH_REMATCH[2]}"

        case "$unit" in
            s) seconds=$num ;;
            m) seconds=$((num * 60)) ;;
            h) seconds=$((num * 3600)) ;;
            d) seconds=$((num * 86400)) ;;
        esac

        # Calculate timestamp
        if date --version &>/dev/null 2>&1; then
            # GNU date
            date -u -d "@$(( $(date +%s) - seconds ))" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null
        else
            # BSD date
            date -u -v-${seconds}S +%Y-%m-%dT%H:%M:%SZ 2>/dev/null
        fi
        return
    fi

    # Return as-is if we can't parse
    echo "$arg"
}

# Format a single log line for display
_needle_format_log_line() {
    local line="$1"

    # Extract fields using jq if available
    if ! _needle_command_exists jq; then
        _needle_print "$line"
        return
    fi

    local ts event session data
    ts=$(echo "$line" | jq -r '.ts // .timestamp // empty' 2>/dev/null)
    event=$(echo "$line" | jq -r '.event // .type // "unknown"' 2>/dev/null)
    session=$(echo "$line" | jq -r '.session // "unknown"' 2>/dev/null)
    data=$(echo "$line" | jq -c '.data // .message // empty' 2>/dev/null)

    # Format timestamp (extract time portion)
    local ts_display="$ts"
    if [[ -n "$ts" ]] && [[ "$ts" =~ T([0-9]{2}:[0-9]{2}:[0-9]{2}) ]]; then
        ts_display="${BASH_REMATCH[1]}"
    fi

    # Shorten session name (remove needle- prefix and common parts)
    local session_display="$session"
    session_display="${session_display#needle-}"
    session_display="${session_display#claude-}"
    session_display="${session_display#anthropic-}"
    session_display="${session_display#sonnet-}"
    session_display="${session_display#opencode-}"
    session_display="${session_display#alibaba-}"
    session_display="${session_display#qwen-}"

    # Determine color based on event type
    local color=""
    local reset="$NEEDLE_COLOR_RESET"

    case "$event" in
        *.completed|*.success)
            color="$NEEDLE_COLOR_GREEN"
            ;;
        *.failed|error.*|*.error)
            color="$NEEDLE_COLOR_RED"
            ;;
        *.started|*.claimed|*.created)
            color="$NEEDLE_COLOR_BLUE"
            ;;
        *.warning|*.warn)
            color="$NEEDLE_COLOR_YELLOW"
            ;;
        heartbeat.*|*.idle)
            color="$NEEDLE_COLOR_DIM"
            ;;
        strand.*|*.fallthrough)
            color="$NEEDLE_COLOR_CYAN"
            ;;
        *)
            color=""
            ;;
    esac

    # Format and print the line
    if [[ -n "$color" ]]; then
        printf '%s %b%-24s%s [%s] %s\n' \
            "$ts_display" \
            "$color" "$event" "$reset" \
            "$session_display" \
            "$data"
    else
        printf '%s %-24s [%s] %s\n' \
            "$ts_display" \
            "$event" \
            "$session_display" \
            "$data"
    fi
}
