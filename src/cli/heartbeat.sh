#!/usr/bin/env bash
# NEEDLE CLI Heartbeat Subcommand
# Manage worker heartbeat and recovery

# Heartbeat pause file path
NEEDLE_HEARTBEAT_PAUSE_FILE="${NEEDLE_HEARTBEAT_PAUSE_FILE:-$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeat-pause}"

_needle_heartbeat_help() {
    _needle_print "Manage worker heartbeat and auto-recovery

Monitor worker health via heartbeats and manage the automatic
recovery watchdog.

USAGE:
    needle heartbeat <COMMAND> [OPTIONS]

COMMANDS:
    status      Show heartbeat status of all workers
    recover     Manually trigger recovery for stuck worker
    pause       Pause automatic recovery
    resume      Resume automatic recovery

OPTIONS:
    -j, --json     Output as JSON
    -h, --help     Print help information

EXAMPLES:
    # Check heartbeat status
    needle heartbeat status

    # Show status as JSON
    needle heartbeat status --json

    # Manually recover stuck worker
    needle heartbeat recover alpha

    # Pause auto-recovery (maintenance)
    needle heartbeat pause

    # Resume auto-recovery
    needle heartbeat resume
"
}

# Main heartbeat command router
_needle_heartbeat() {
    local subcommand="${1:-status}"
    shift || true

    case "$subcommand" in
        status)
            _needle_heartbeat_status "$@"
            ;;
        recover)
            _needle_heartbeat_recover_cmd "$@"
            ;;
        pause)
            _needle_heartbeat_pause "$@"
            ;;
        resume)
            _needle_heartbeat_resume "$@"
            ;;
        -j|--json)
            # Allow --json as first argument for status
            _needle_heartbeat_status "$subcommand" "$@"
            ;;
        -w|--watch)
            # Allow --watch as first argument for status
            _needle_heartbeat_status "$subcommand" "$@"
            ;;
        -h|--help|help)
            _needle_heartbeat_help
            exit $NEEDLE_EXIT_SUCCESS
            ;;
        *)
            _needle_error "Unknown subcommand: $subcommand"
            _needle_heartbeat_help
            exit $NEEDLE_EXIT_USAGE
            ;;
    esac
}

# Show heartbeat status for all workers
_needle_heartbeat_status() {
    local json_output=false
    local watch=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -j|--json)
                json_output=true
                shift
                ;;
            -w|--watch)
                watch=true
                shift
                ;;
            -h|--help)
                _needle_heartbeat_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                shift
                ;;
        esac
    done

    if [[ "$watch" == "true" ]]; then
        # Watch mode: clear and refresh
        while true; do
            clear
            _needle_heartbeat_status_display "$json_output"
            sleep 2
        done
    else
        _needle_heartbeat_status_display "$json_output"
    fi

    exit $NEEDLE_EXIT_SUCCESS
}

# Main display function for heartbeat status
_needle_heartbeat_status_display() {
    local json_output="${1:-false}"

    # Get heartbeat directory
    local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
    local pause_active=false

    # Check if pause is active
    if [[ -f "$NEEDLE_HEARTBEAT_PAUSE_FILE" ]]; then
        pause_active=true
    fi

    # Get timeout from config (default 120 seconds)
    local timeout
    timeout=$(_needle_config_get "heartbeat.timeout" "120" 2>/dev/null || echo "120")
    # Remove 's' suffix if present
    timeout="${timeout%s}"

    local now
    now=$(date +%s)

    # Collect heartbeat data
    local heartbeat_files=()
    if [[ -d "$heartbeat_dir" ]]; then
        while IFS= read -r -d '' file; do
            heartbeat_files+=("$file")
        done < <(find "$heartbeat_dir" -name "*.json" -type f -print0 2>/dev/null)
    fi

    if $json_output; then
        _needle_heartbeat_status_json "$heartbeat_dir" "$timeout" "$now" "$pause_active" "${heartbeat_files[@]}"
    else
        _needle_heartbeat_status_dashboard "$heartbeat_dir" "$timeout" "$now" "$pause_active" "${heartbeat_files[@]}"
    fi
}

# Output status as JSON
_needle_heartbeat_status_json() {
    local heartbeat_dir="$1"
    local timeout="$2"
    local now="$3"
    local pause_active="$4"
    shift 4
    local -a heartbeat_files=("$@")

    local workers_json="[]"

    if [[ ${#heartbeat_files[@]} -eq 0 ]]; then
        jq -n \
            --argjson paused "$pause_active" \
            --argjson timeout "$timeout" \
            '{paused: $paused, timeout: $timeout, workers: []}'
        return 0
    fi

    local workers=()
    for heartbeat_file in "${heartbeat_files[@]}"; do
        [[ -f "$heartbeat_file" ]] || continue

        local worker status last_heartbeat current_bead bead_started
        worker=$(jq -r '.worker // "unknown"' "$heartbeat_file" 2>/dev/null)
        status=$(jq -r '.status // "unknown"' "$heartbeat_file" 2>/dev/null)
        last_heartbeat=$(jq -r '.last_heartbeat // ""' "$heartbeat_file" 2>/dev/null)
        current_bead=$(jq -r '.current_bead // ""' "$heartbeat_file" 2>/dev/null)
        bead_started=$(jq -r '.bead_started // ""' "$heartbeat_file" 2>/dev/null)
        local pid workspace agent
        pid=$(jq -r '.pid // 0' "$heartbeat_file" 2>/dev/null)
        workspace=$(jq -r '.workspace // ""' "$heartbeat_file" 2>/dev/null)
        agent=$(jq -r '.agent // "unknown"' "$heartbeat_file" 2>/dev/null)

        # Calculate ages
        local heartbeat_epoch=0
        if [[ -n "$last_heartbeat" ]]; then
            heartbeat_epoch=$(_needle_parse_iso_timestamp "$last_heartbeat")
        fi
        local heartbeat_ago=$((now - heartbeat_epoch))

        local bead_duration=""
        local bead_epoch=0
        if [[ -n "$bead_started" && "$bead_started" != "null" ]]; then
            bead_epoch=$(_needle_parse_iso_timestamp "$bead_started")
            bead_duration=$((now - bead_epoch))
        fi

        # Determine health
        local health="healthy"
        if ((heartbeat_ago > timeout)); then
            health="STUCK"
        elif ((heartbeat_ago > timeout / 2)); then
            health="warning"
        fi

        # Check if process is still alive
        local alive=true
        if [[ "$pid" -gt 0 ]] && ! kill -0 "$pid" 2>/dev/null; then
            alive=false
            health="DEAD"
        fi

        workers+=("$(jq -nc \
            --arg worker "$worker" \
            --arg status "$status" \
            --arg health "$health" \
            --argjson heartbeat_ago "$heartbeat_ago" \
            --arg current_bead "$current_bead" \
            --argjson bead_duration "${bead_duration:-0}" \
            --argjson pid "$pid" \
            --arg workspace "$workspace" \
            --arg agent "$agent" \
            --argjson alive "$alive" \
            '{
                worker: $worker,
                status: $status,
                health: $health,
                last_heartbeat_ago: $heartbeat_ago,
                current_bead: (if $current_bead == "" then null else $current_bead end),
                bead_duration: (if $bead_duration == 0 then null else $bead_duration end),
                pid: $pid,
                workspace: (if $workspace == "" then null else $workspace end),
                agent: $agent,
                alive: $alive
            }'
        )")
    done

    # Combine all workers into a single JSON output
    if [[ ${#workers[@]} -gt 0 ]]; then
        printf '%s\n' "${workers[@]}" | jq -s \
            --argjson paused "$pause_active" \
            --argjson timeout "$timeout" \
            '{paused: $paused, timeout: $timeout, workers: .}'
    else
        jq -n \
            --argjson paused "$pause_active" \
            --argjson timeout "$timeout" \
            '{paused: $paused, timeout: $timeout, workers: []}'
    fi
}

# Output status as dashboard
_needle_heartbeat_status_dashboard() {
    local heartbeat_dir="$1"
    local timeout="$2"
    local now="$3"
    local pause_active="$4"
    shift 4
    local -a heartbeat_files=("$@")

    # Header
    local header_width=80
    _needle_print ""
    _needle_print "$(printf '═%.0s' $(seq 1 $header_width))"
    _needle_print_color "$NEEDLE_COLOR_BOLD" "$(printf '%*s' $(((header_width - 17) / 2 + 8)) 'HEARTBEAT STATUS')"
    _needle_print "$(printf '═%.0s' $(seq 1 $header_width))"
    _needle_print ""

    # Show pause status
    if $pause_active; then
        _needle_print_color "$NEEDLE_COLOR_YELLOW" "  ⏸ Auto-recovery is PAUSED"
    else
        _needle_print_color "$NEEDLE_COLOR_GREEN" "  ▶ Auto-recovery is ACTIVE"
    fi
    _needle_print "  Timeout: ${timeout}s"
    _needle_print ""

    if [[ ${#heartbeat_files[@]} -eq 0 ]]; then
        _needle_print "  No worker heartbeats found"
        _needle_print ""
        _needle_print "$(printf '═%.0s' $(seq 1 $header_width))"
        return 0
    fi

    # Table header
    _needle_print_color "$NEEDLE_COLOR_BOLD" "  WORKER                                          HEALTH     LAST BEAT    BEAD           BEAD TIME"
    _needle_print_color "$NEEDLE_COLOR_DIM" "  ───────────────────────────────────────────────  ──────────  ───────────  ─────────────  ──────────"

    local stuck_count=0
    local warning_count=0
    local healthy_count=0

    for heartbeat_file in "${heartbeat_files[@]}"; do
        [[ -f "$heartbeat_file" ]] || continue

        local worker status last_heartbeat current_bead bead_started
        worker=$(jq -r '.worker // "unknown"' "$heartbeat_file" 2>/dev/null)
        status=$(jq -r '.status // "unknown"' "$heartbeat_file" 2>/dev/null)
        last_heartbeat=$(jq -r '.last_heartbeat // ""' "$heartbeat_file" 2>/dev/null)
        current_bead=$(jq -r '.current_bead // ""' "$heartbeat_file" 2>/dev/null)
        bead_started=$(jq -r '.bead_started // ""' "$heartbeat_file" 2>/dev/null)

        # Calculate ages
        local heartbeat_epoch=0
        if [[ -n "$last_heartbeat" ]]; then
            heartbeat_epoch=$(_needle_parse_iso_timestamp "$last_heartbeat")
        fi
        local heartbeat_ago=$((now - heartbeat_epoch))

        local bead_duration=""
        if [[ -n "$bead_started" && "$bead_started" != "null" ]]; then
            local bead_epoch
            bead_epoch=$(_needle_parse_iso_timestamp "$bead_started")
            bead_duration="$((now - bead_epoch))s"
        fi

        # Determine health
        local health="healthy"
        local health_color="$NEEDLE_COLOR_GREEN"
        if ((heartbeat_ago > timeout)); then
            health="STUCK"
            health_color="$NEEDLE_COLOR_RED"
            ((stuck_count++))
        elif ((heartbeat_ago > timeout / 2)); then
            health="warning"
            health_color="$NEEDLE_COLOR_YELLOW"
            ((warning_count++))
        else
            ((healthy_count++))
        fi

        # Format heartbeat ago
        local heartbeat_ago_str
        heartbeat_ago_str=$(_needle_format_duration "$heartbeat_ago")

        # Format worker name (truncate to 45 chars)
        local display_worker="${worker:0:45}"
        if [[ ${#worker} -gt 45 ]]; then
            display_worker="${display_worker}…"
        fi

        # Format current bead
        local display_bead="${current_bead:-(idle)}"
        if [[ ${#display_bead} -gt 12 ]]; then
            display_bead="${display_bead:0:12}…"
        fi

        # Print row with color-coded health
        printf "  %-45s  " "$display_worker"
        _needle_print_color "$health_color" "$(printf '%-10s' "$health")"
        printf "  %-11s  %-13s %s\n" "${heartbeat_ago_str} ago" "$display_bead" "${bead_duration:--}"
    done

    _needle_print ""

    # Summary
    _needle_print_color "$NEEDLE_COLOR_BOLD" "  SUMMARY"
    _needle_print "  ────────────────────────────────────────────────────────────────────────────────"
    _needle_print "  Healthy: $healthy_count    Warning: $warning_count    Stuck: $stuck_count"

    if [[ $stuck_count -gt 0 ]]; then
        _needle_print ""
        _needle_print_color "$NEEDLE_COLOR_YELLOW" "  Run 'needle heartbeat recover' to trigger manual recovery for stuck workers"
    fi

    _needle_print ""
    _needle_print "$(printf '═%.0s' $(seq 1 $header_width))"
}

# Parse ISO timestamp to epoch seconds
_needle_parse_iso_timestamp() {
    local timestamp="$1"

    if [[ -z "$timestamp" ]] || [[ "$timestamp" == "null" ]]; then
        echo "0"
        return
    fi

    # Try GNU date first
    if date --version &>/dev/null; then
        date -d "$timestamp" +%s 2>/dev/null && return
    fi

    # Try BSD date
    if date -j -f "%Y-%m-%dT%H:%M:%SZ" "$timestamp" +%s 2>/dev/null; then
        return
    fi

    # Manual parsing fallback (format: 2026-03-02T01:23:45Z)
    if [[ "$timestamp" =~ ^([0-9]{4})-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2}) ]]; then
        date -d "${BASH_REMATCH[1]}-${BASH_REMATCH[2]}-${BASH_REMATCH[3]} ${BASH_REMATCH[4]}:${BASH_REMATCH[5]}:${BASH_REMATCH[6]}" +%s 2>/dev/null && return
    fi

    echo "0"
}

# Format duration in human-readable format
_needle_format_duration() {
    local seconds="$1"

    if [[ $seconds -lt 0 ]]; then
        echo "?"
        return
    fi

    if [[ $seconds -lt 60 ]]; then
        echo "${seconds}s"
    elif [[ $seconds -lt 3600 ]]; then
        echo "$((seconds / 60))m $((seconds % 60))s"
    elif [[ $seconds -lt 86400 ]]; then
        echo "$((seconds / 3600))h $(((seconds % 3600) / 60))m"
    else
        echo "$((seconds / 86400))d $(((seconds % 86400) / 3600))h"
    fi
}

# Trigger manual recovery
_needle_heartbeat_recover_cmd() {
    local force=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            -f|--force)
                force=true
                shift
                ;;
            -h|--help)
                _needle_print "Trigger manual recovery for stuck workers

USAGE:
    needle heartbeat recover [OPTIONS]

OPTIONS:
    -f, --force    Force recovery even if paused
    -h, --help     Show this help message

This command identifies workers with stale heartbeats and triggers
recovery actions. Recovery typically involves:
  - Closing stuck beads
  - Cleaning up heartbeat files
  - Notifying monitoring systems
"
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                shift
                ;;
        esac
    done

    # Check if paused
    if [[ -f "$NEEDLE_HEARTBEAT_PAUSE_FILE" ]] && ! $force; then
        _needle_warn "Auto-recovery is paused. Use --force to override."
        exit $NEEDLE_EXIT_ERROR
    fi

    local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
    local timeout
    timeout=$(_needle_config_get "heartbeat.timeout" "120" 2>/dev/null || echo "120")
    timeout="${timeout%s}"

    local now
    now=$(date +%s)

    local recovered=0
    local checked=0

    _needle_info "Scanning for stuck workers (timeout: ${timeout}s)..."

    if [[ ! -d "$heartbeat_dir" ]]; then
        _needle_info "No heartbeat directory found"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    for heartbeat_file in "$heartbeat_dir"/*.json; do
        [[ -f "$heartbeat_file" ]] || continue
        ((checked++))

        local worker last_heartbeat current_bead
        worker=$(jq -r '.worker // "unknown"' "$heartbeat_file" 2>/dev/null)
        last_heartbeat=$(jq -r '.last_heartbeat // ""' "$heartbeat_file" 2>/dev/null)
        current_bead=$(jq -r '.current_bead // ""' "$heartbeat_file" 2>/dev/null)

        # Calculate age
        local heartbeat_epoch=0
        if [[ -n "$last_heartbeat" ]]; then
            heartbeat_epoch=$(_needle_parse_iso_timestamp "$last_heartbeat")
        fi
        local heartbeat_ago=$((now - heartbeat_epoch))

        # Check if stuck
        if ((heartbeat_ago > timeout)); then
            _needle_warn "Recovering stuck worker: $worker (last beat: ${heartbeat_ago}s ago)"

            # If worker has a current bead, we should close it
            if [[ -n "$current_bead" && "$current_bead" != "null" ]]; then
                _needle_info "  Closing stuck bead: $current_bead"

                # Use br CLI to close the bead if available
                if command -v br &>/dev/null; then
                    br close "$current_bead" --reason "Stuck worker recovered (timeout: ${timeout}s)" 2>/dev/null || true
                fi
            fi

            # Remove stale heartbeat file
            rm -f "$heartbeat_file"
            _needle_success "  Removed stale heartbeat: $(basename "$heartbeat_file")"
            ((recovered++))
        fi
    done

    _needle_print ""
    if [[ $checked -eq 0 ]]; then
        _needle_info "No worker heartbeats found"
    elif [[ $recovered -eq 0 ]]; then
        _needle_success "No stuck workers found (checked $checked)"
    else
        _needle_success "Recovered $recovered stuck worker(s)"
    fi

    exit $NEEDLE_EXIT_SUCCESS
}

# Pause automatic recovery
_needle_heartbeat_pause() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -h|--help)
                _needle_print "Pause automatic recovery

USAGE:
    needle heartbeat pause [OPTIONS]

OPTIONS:
    -h, --help     Show this help message

Creates a pause file that prevents the watchdog from automatically
recovering stuck workers. Useful during maintenance or debugging.
"
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                shift
                ;;
        esac
    done

    # Ensure state directory exists
    local state_dir
    state_dir=$(dirname "$NEEDLE_HEARTBEAT_PAUSE_FILE")
    mkdir -p "$state_dir" 2>/dev/null

    if [[ -f "$NEEDLE_HEARTBEAT_PAUSE_FILE" ]]; then
        _needle_warn "Auto-recovery is already paused"
    else
        # Create pause file with timestamp
        echo "{\"paused_at\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"reason\":\"manual\"}" > "$NEEDLE_HEARTBEAT_PAUSE_FILE"
        _needle_success "Auto-recovery paused"
        _needle_info "Run 'needle heartbeat resume' to resume"
    fi

    exit $NEEDLE_EXIT_SUCCESS
}

# Resume automatic recovery
_needle_heartbeat_resume() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -h|--help)
                _needle_print "Resume automatic recovery

USAGE:
    needle heartbeat resume [OPTIONS]

OPTIONS:
    -h, --help     Show this help message

Removes the pause file and allows the watchdog to automatically
recover stuck workers.
"
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                shift
                ;;
        esac
    done

    if [[ ! -f "$NEEDLE_HEARTBEAT_PAUSE_FILE" ]]; then
        _needle_info "Auto-recovery is already active"
    else
        rm -f "$NEEDLE_HEARTBEAT_PAUSE_FILE"
        _needle_success "Auto-recovery resumed"
    fi

    exit $NEEDLE_EXIT_SUCCESS
}
