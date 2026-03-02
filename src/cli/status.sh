#!/usr/bin/env bash
# NEEDLE CLI Status Subcommand
# Show current status and health of NEEDLE with dashboard view

_needle_status_help() {
    _needle_print "Show current status and health of NEEDLE

Displays a comprehensive dashboard with information about workers,
beads, strands, and effort metrics.

USAGE:
    needle status [OPTIONS]

OPTIONS:
    -w, --watch       Auto-refresh display every 2 seconds
    -j, --json        Output in JSON format
    -h, --help        Show this help message

EXAMPLES:
    # Show status dashboard
    needle status

    # Continuous monitoring
    needle status --watch

    # Output as JSON for scripting
    needle status --json
"
}

_needle_status() {
    local watch=false
    local json_output=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -w|--watch)
                watch=true
                shift
                ;;
            -j|--json)
                json_output=true
                shift
                ;;
            -h|--help)
                _needle_status_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                _needle_error "Unknown option: $1"
                _needle_status_help
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    if [[ "$watch" == "true" ]]; then
        # Watch mode: clear and refresh
        while true; do
            clear
            _needle_status_display "$json_output"
            sleep 2
        done
    else
        _needle_status_display "$json_output"
    fi

    exit $NEEDLE_EXIT_SUCCESS
}

# Main display function
_needle_status_display() {
    local json_output="${1:-false}"

    # Collect all data first
    local workers_json workers_count
    workers_json=$(_needle_status_get_workers)
    workers_count=$(echo "$workers_json" | jq '. | length' 2>/dev/null || echo "0")

    local beads_json
    beads_json=$(_needle_status_get_beads)

    local strands_json
    strands_json=$(_needle_status_get_strands)

    local effort_json
    effort_json=$(_needle_status_get_effort)

    local workspace
    workspace=$(_needle_status_get_workspace)

    if [[ "$json_output" == "true" ]]; then
        _needle_status_output_json "$workers_json" "$beads_json" "$strands_json" "$effort_json" "$workspace"
    else
        _needle_status_output_dashboard "$workers_json" "$workers_count" "$beads_json" "$strands_json" "$effort_json" "$workspace"
    fi
}

# Get workers from state registry
_needle_status_get_workers() {
    if [[ ! -f "$NEEDLE_WORKERS_FILE" ]]; then
        echo "[]"
        return 0
    fi

    # Clean up stale workers first
    _needle_cleanup_stale_workers 2>/dev/null || true

    # Return workers array
    jq '.workers' "$NEEDLE_WORKERS_FILE" 2>/dev/null || echo "[]"
}

# Get bead statistics using br CLI
_needle_status_get_beads() {
    # Check if br is available
    if ! command -v br &>/dev/null; then
        echo '{"open":0,"in_progress":0,"completed":0,"failed":0,"blocked":0,"quarantined":0,"today_completed":0}'
        return 0
    fi

    # Get stats from br
    local br_stats
    br_stats=$(br stats --json 2>/dev/null) || {
        echo '{"open":0,"in_progress":0,"completed":0,"failed":0,"blocked":0,"quarantined":0,"today_completed":0}'
        return 0
    }

    # Extract relevant fields
    local open in_progress closed blocked today_completed

    open=$(echo "$br_stats" | jq -r '.summary.open_issues // 0' 2>/dev/null || echo "0")
    in_progress=$(echo "$br_stats" | jq -r '.summary.in_progress_issues // 0' 2>/dev/null || echo "0")
    closed=$(echo "$br_stats" | jq -r '.summary.closed_issues // 0' 2>/dev/null || echo "0")
    blocked=$(echo "$br_stats" | jq -r '.summary.blocked_issues // 0' 2>/dev/null || echo "0")

    # Count failed beads (closed with 'failed' label)
    local failed
    failed=$(br count --status closed --label failed 2>/dev/null | grep -oE '[0-9]+' || echo "0")

    # Count quarantined (closed with 'quarantined' label)
    local quarantined
    quarantined=$(br count --status closed --label quarantined 2>/dev/null | grep -oE '[0-9]+' || echo "0")

    # Today's completed - from recent activity (simplified - use closed in last 24h)
    today_completed=$(echo "$br_stats" | jq -r '.recent_activity.issues_closed // 0' 2>/dev/null || echo "0")

    # Build JSON
    jq -n \
        --argjson open "$open" \
        --argjson in_progress "$in_progress" \
        --argjson completed "$closed" \
        --argjson failed "$failed" \
        --argjson blocked "$blocked" \
        --argjson quarantined "$quarantined" \
        --argjson today_completed "$today_completed" \
        '{open: $open, in_progress: $in_progress, completed: $completed, failed: $failed, blocked: $blocked, quarantined: $quarantined, today_completed: $today_completed}'
}

# Get strand status from config
_needle_status_get_strands() {
    local config
    config=$(load_config 2>/dev/null || echo "$_NEEDLE_CONFIG_DEFAULTS")

    # Extract strand settings
    local pluck explore mend weave unravel pulse knot

    pluck=$(echo "$config" | jq -r '.strands.pluck // false' 2>/dev/null || echo "false")
    explore=$(echo "$config" | jq -r '.strands.explore // false' 2>/dev/null || echo "false")
    mend=$(echo "$config" | jq -r '.strands.mend // false' 2>/dev/null || echo "false")
    weave=$(echo "$config" | jq -r '.strands.weave // false' 2>/dev/null || echo "false")
    unravel=$(echo "$config" | jq -r '.strands.unravel // false' 2>/dev/null || echo "false")
    pulse=$(echo "$config" | jq -r '.strands.pulse // false' 2>/dev/null || echo "false")
    knot=$(echo "$config" | jq -r '.strands.knot // false' 2>/dev/null || echo "false")

    # Build JSON with status (active/idle/disabled based on config and current activity)
    jq -n \
        --arg pluck "$pluck" \
        --arg explore "$explore" \
        --arg mend "$mend" \
        --arg weave "$weave" \
        --arg unravel "$unravel" \
        --arg pulse "$pulse" \
        --arg knot "$knot" \
        '{
            pluck: (if $pluck == "true" then "idle" else "disabled" end),
            explore: (if $explore == "true" then "idle" else "disabled" end),
            mend: (if $mend == "true" then "idle" else "disabled" end),
            weave: (if $weave == "true" then "idle" else "disabled" end),
            unravel: (if $unravel == "true" then "idle" else "disabled" end),
            pulse: (if $pulse == "true" then "idle" else "disabled" end),
            knot: (if $knot == "true" then "idle" else "disabled" end)
        }'
}

# Get effort metrics from telemetry logs
_needle_status_get_effort() {
    local log_dir="$NEEDLE_HOME/$NEEDLE_LOG_DIR"
    local today_tokens=0
    local today_cost="0.00"

    # Look for today's telemetry log
    if [[ -d "$log_dir" ]]; then
        # Sum tokens from all log files (simplified - in reality would parse JSONL)
        # For now, return placeholder values that would be populated by actual telemetry
        local today_log="$log_dir/$(date +%Y-%m-%d).jsonl"
        if [[ -f "$today_log" ]]; then
            # Count events as a proxy for activity
            local event_count
            event_count=$(wc -l < "$today_log" 2>/dev/null || echo "0")
            # Placeholder calculation - real implementation would sum actual token counts
            today_tokens=$((event_count * 1000))
        fi
    fi

    jq -n \
        --argjson tokens "$today_tokens" \
        --arg cost "$today_cost" \
        '{tokens: $tokens, cost: $cost}'
}

# Get current workspace
_needle_status_get_workspace() {
    # Try to find workspace from environment or current directory
    if [[ -n "${NEEDLE_WORKSPACE:-}" ]]; then
        echo "$NEEDLE_WORKSPACE"
    elif [[ -n "${WORKSPACE:-}" ]]; then
        echo "$WORKSPACE"
    else
        pwd
    fi
}

# Output JSON format
_needle_status_output_json() {
    local workers_json="$1"
    local beads_json="$2"
    local strands_json="$3"
    local effort_json="$4"
    local workspace="$5"

    local initialized="false"
    local config_exists="false"
    local state_dir_exists="false"
    local cache_dir_exists="false"

    if _needle_is_initialized; then
        initialized="true"
        [[ -f "$NEEDLE_HOME/$NEEDLE_CONFIG_FILE" ]] && config_exists="true"
        [[ -d "$NEEDLE_HOME/$NEEDLE_STATE_DIR" ]] && state_dir_exists="true"
        [[ -d "$NEEDLE_HOME/$NEEDLE_CACHE_DIR" ]] && cache_dir_exists="true"
    fi

    jq -n \
        --arg version "$NEEDLE_VERSION" \
        --arg home "$NEEDLE_HOME" \
        --argjson initialized "$initialized" \
        --arg workspace "$workspace" \
        --argjson workers "$workers_json" \
        --argjson beads "$beads_json" \
        --argjson strands "$strands_json" \
        --argjson effort "$effort_json" \
        '{
            version: $version,
            home: $home,
            initialized: $initialized,
            workspace: $workspace,
            workers: $workers,
            beads: $beads,
            strands: $strands,
            effort: $effort
        }'
}

# Output dashboard format
_needle_status_output_dashboard() {
    local workers_json="$1"
    local workers_count="$2"
    local beads_json="$3"
    local strands_json="$4"
    local effort_json="$5"
    local workspace="$6"

    # Header
    local header_width=63
    _needle_print ""
    _needle_print "$(printf '═%.0s' $(seq 1 $header_width))"
    _needle_print_color "$NEEDLE_COLOR_BOLD" "$(printf '%*s' $(((header_width - 13) / 2 + 6)) 'NEEDLE STATUS')"
    _needle_print "$(printf '═%.0s' $(seq 1 $header_width))"
    _needle_print ""

    # WORKERS section
    _needle_status_display_workers "$workers_json" "$workers_count"

    # BEADS section
    _needle_status_display_beads "$beads_json" "$workspace"

    # STRANDS section
    _needle_status_display_strands "$strands_json"

    # EFFORT section
    _needle_status_display_effort "$effort_json"

    # Footer
    _needle_print ""
    _needle_print "$(printf '═%.0s' $(seq 1 $header_width))"
    _needle_print ""
}

# Display workers section
_needle_status_display_workers() {
    local workers_json="$1"
    local workers_count="$2"

    _needle_print_color "$NEEDLE_COLOR_BOLD" "WORKERS ($workers_count active)"

    if [[ "$workers_count" -eq 0 ]]; then
        _needle_print "  No active workers"
    else
        # Display each worker
        echo "$workers_json" | jq -r '.[] | "\(.session) \(.runner) \(.provider) \(.model) \(.identifier) \(.started)"' 2>/dev/null | while read -r session runner provider model identifier started; do
            # Calculate runtime
            local runtime=""
            if [[ -n "$started" ]]; then
                runtime=$(_needle_status_format_runtime "$started")
            fi

            # Format worker line (truncate long session names)
            local display_session="${session:0:40}"
            if [[ ${#session} -gt 40 ]]; then
                display_session="${display_session}..."
            fi

            _needle_print "  $display_session  $runtime"
        done
    fi

    _needle_print ""
}

# Display beads section
_needle_status_display_beads() {
    local beads_json="$1"
    local workspace="$2"

    # Extract values
    local open in_progress completed failed quarantined today_completed blocked

    open=$(echo "$beads_json" | jq -r '.open // 0' 2>/dev/null || echo "0")
    in_progress=$(echo "$beads_json" | jq -r '.in_progress // 0' 2>/dev/null || echo "0")
    completed=$(echo "$beads_json" | jq -r '.completed // 0' 2>/dev/null || echo "0")
    failed=$(echo "$beads_json" | jq -r '.failed // 0' 2>/dev/null || echo "0")
    quarantined=$(echo "$beads_json" | jq -r '.quarantined // 0' 2>/dev/null || echo "0")
    today_completed=$(echo "$beads_json" | jq -r '.today_completed // 0' 2>/dev/null || echo "0")
    blocked=$(echo "$beads_json" | jq -r '.blocked // 0' 2>/dev/null || echo "0")

    _needle_print_color "$NEEDLE_COLOR_BOLD" "BEADS (workspace: $workspace)"

    # Generate mini bar charts
    local total=$((open + in_progress + completed + failed + blocked))
    local bar_width=10

    local open_bar in_progress_bar
    open_bar=$(_needle_status_mini_bar "$open" "$total" "$bar_width")
    in_progress_bar=$(_needle_status_mini_bar "$in_progress" "$total" "$bar_width")

    _needle_print "  Open:        $(printf '%3s' "$open")     $open_bar"
    _needle_print "  In Progress: $(printf '%3s' "$in_progress")     $in_progress_bar"
    _needle_print "  Completed:   $(printf '%3s' "$completed")     (today: $today_completed)"

    if [[ "$failed" -gt 0 ]] || [[ "$quarantined" -gt 0 ]]; then
        _needle_print "  Failed:      $(printf '%3s' "$failed")     (quarantined: $quarantined)"
    fi

    _needle_print ""
}

# Display strands section
_needle_status_display_strands() {
    local strands_json="$1"

    _needle_print_color "$NEEDLE_COLOR_BOLD" "STRANDS"

    # Strand order
    local strands=("pluck" "explore" "mend" "weave" "unravel" "pulse" "knot")

    for strand in "${strands[@]}"; do
        local status
        status=$(echo "$strands_json" | jq -r ".$strand // \"disabled\"" 2>/dev/null || echo "disabled")

        # Color-code status
        local status_display
        case "$status" in
            active)
                status_display="$NEEDLE_COLOR_GREEN$status$NEEDLE_COLOR_RESET"
                ;;
            idle)
                status_display="$NEEDLE_COLOR_DIM$status$NEEDLE_COLOR_RESET"
                ;;
            *)
                status_display="$NEEDLE_COLOR_DIM$status$NEEDLE_COLOR_RESET"
                ;;
        esac

        _needle_print "  $(printf '%-10s' "$strand:")  $status_display"
    done

    _needle_print ""
}

# Display effort section
_needle_status_display_effort() {
    local effort_json="$1"

    local tokens cost
    tokens=$(echo "$effort_json" | jq -r '.tokens // 0' 2>/dev/null || echo "0")
    cost=$(echo "$effort_json" | jq -r '.cost // "0.00"' 2>/dev/null || echo "0.00")

    # Format tokens with commas
    local formatted_tokens
    formatted_tokens=$(printf "%'d" "$tokens" 2>/dev/null || echo "$tokens")

    _needle_print_color "$NEEDLE_COLOR_BOLD" "EFFORT (today)"
    _needle_print "  Tokens:  $formatted_tokens"
    _needle_print "  Cost:    \$$cost"
}

# Generate mini bar chart
_needle_status_mini_bar() {
    local value="$1"
    local total="$2"
    local width="${3:-10}"

    if [[ "$total" -eq 0 ]]; then
        printf '░%.0s' $(seq 1 $width)
        return
    fi

    local filled=$((value * width / total))
    local empty=$((width - filled))

    local bar=""
    if [[ $filled -gt 0 ]]; then
        bar+=$(printf '█%.0s' $(seq 1 $filled))
    fi
    if [[ $empty -gt 0 ]]; then
        bar+=$(printf '░%.0s' $(seq 1 $empty))
    fi

    echo "$bar"
}

# Format runtime from ISO timestamp
_needle_status_format_runtime() {
    local started="$1"

    # Parse ISO timestamp and calculate difference
    # Simplified implementation - just shows relative time
    local now=$(date +%s)
    local started_epoch

    # Try to parse the timestamp (format: 2026-03-02T01:23:45Z)
    if [[ "$started" =~ ^([0-9]{4})-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2}) ]]; then
        started_epoch=$(date -d "${BASH_REMATCH[1]}-${BASH_REMATCH[2]}-${BASH_REMATCH[3]} ${BASH_REMATCH[4]}:${BASH_REMATCH[5]}:${BASH_REMATCH[6]}" +%s 2>/dev/null || echo "$now")
    else
        echo "?"
        return
    fi

    local diff=$((now - started_epoch))

    if [[ $diff -lt 60 ]]; then
        echo "${diff}s"
    elif [[ $diff -lt 3600 ]]; then
        echo "$((diff / 60))m"
    elif [[ $diff -lt 86400 ]]; then
        echo "$((diff / 3600))h $(((diff % 3600) / 60))m"
    else
        echo "$((diff / 86400))d $(((diff % 86400) / 3600))h"
    fi
}
