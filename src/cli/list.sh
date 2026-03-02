#!/usr/bin/env bash
# NEEDLE CLI List Subcommand
# List running workers with status, bead, and workspace information

_needle_list_help() {
    _needle_print "List running workers

Shows information about active NEEDLE workers including their status,
current bead, duration, and workspace.

USAGE:
    needle list [OPTIONS]

OPTIONS:
    -a, --all              Include stopped/crashed workers
    -j, --json             Output as JSON
    -w, --wide             Show extended information (PID, started, agent)
    --runner <NAME>        Filter by runner (e.g., claude, opencode)
    --provider <NAME>      Filter by provider (e.g., anthropic, alibaba)
    --model <NAME>         Filter by model (e.g., sonnet, qwen)
    --workspace <PATH>     Filter by workspace path
    -q, --quiet            Only show session names (one per line)
    -h, --help             Show this help message

OUTPUT COLUMNS:
    WORKER     Full session name (needle-{runner}-{provider}-{model}-{id})
    STATUS     Current status: idle, executing, draining, starting, unknown
    BEAD       Current bead being processed (or \"(idle)\" if none)
    DURATION   Time spent on current bead
    WORKSPACE  Workspace directory path

EXAMPLES:
    # List all running workers
    needle list

    # Output as JSON for scripting
    needle list --json

    # Show extended information
    needle list --wide

    # Filter by runner
    needle list --runner claude

    # Filter by provider
    needle list --provider anthropic

    # Filter by model
    needle list --model sonnet

    # Filter by workspace
    needle list --workspace /home/coder/project

    # Include stopped workers
    needle list --all

    # Get just session names for scripting
    needle list --quiet
"
}

# Calculate duration between two ISO8601 timestamps
# Usage: _needle_list_duration <start_timestamp> [end_timestamp]
# Returns: Human readable duration like "5m 23s" or "12m 05s"
_needle_list_duration() {
    local start_ts="$1"
    local end_ts="${2:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}"

    if [[ -z "$start_ts" ]]; then
        echo "-"
        return 0
    fi

    # Convert to epoch seconds (compatible with both GNU and BSD date)
    local start_epoch end_epoch
    if date --version &>/dev/null; then
        # GNU date
        start_epoch=$(date -d "$start_ts" +%s 2>/dev/null) || { echo "-"; return 0; }
        end_epoch=$(date -d "$end_ts" +%s 2>/dev/null) || end_epoch=$(date +%s)
    else
        # BSD date
        start_epoch=$(date -j -f "%Y-%m-%dT%H:%M:%SZ" "$start_ts" +%s 2>/dev/null) || { echo "-"; return 0; }
        end_epoch=$(date -j -f "%Y-%m-%dT%H:%M:%SZ" "$end_ts" +%s 2>/dev/null) || end_epoch=$(date +%s)
    fi

    local diff=$((end_epoch - start_epoch))

    if [[ $diff -lt 0 ]]; then
        echo "-"
        return 0
    fi

    local days=$((diff / 86400))
    local hours=$(( (diff % 86400) / 3600 ))
    local minutes=$(( (diff % 3600) / 60 ))
    local seconds=$((diff % 60))

    local result=""
    if [[ $days -gt 0 ]]; then
        result="${days}d ${hours}h ${minutes}m"
    elif [[ $hours -gt 0 ]]; then
        result="${hours}h ${minutes}m ${seconds}s"
    elif [[ $minutes -gt 0 ]]; then
        result="${minutes}m ${seconds}s"
    else
        result="${seconds}s"
    fi

    printf "%s" "$result"
}

# Format output as a table
_needle_list_table() {
    local workers=("$@")
    local wide="$NEEDLE_LIST_WIDE"

    if [[ ${#workers[@]} -eq 0 ]]; then
        return 0
    fi

    if [[ "$wide" == "true" ]]; then
        # Wide format: include PID, started, agent
        printf "%-45s %-10s %-12s %-10s %-30s %-7s %-20s %s\n" \
            "WORKER" "STATUS" "BEAD" "DURATION" "WORKSPACE" "PID" "STARTED" "AGENT"
        printf "%-45s %-10s %-12s %-10s %-30s %-7s %-20s %s\n" \
            "------" "------" "----" "--------" "---------" "---" "-------" "-----"

        for worker in "${workers[@]}"; do
            IFS=$'\t' read -r session status bead duration workspace pid started agent <<< "$worker"
            printf "%-45s %-10s %-12s %-10s %-30s %-7s %-20s %s\n" \
                "$session" \
                "$status" \
                "${bead:-(idle)}" \
                "${duration:--}" \
                "${workspace:--}" \
                "${pid:--}" \
                "${started:--}" \
                "${agent:--}"
        done
    else
        # Standard format
        printf "%-45s %-10s %-12s %-10s %s\n" "WORKER" "STATUS" "BEAD" "DURATION" "WORKSPACE"
        printf "%-45s %-10s %-12s %-10s %s\n" "------" "------" "----" "--------" "---------"

        for worker in "${workers[@]}"; do
            IFS=$'\t' read -r session status bead duration workspace <<< "$worker"
            printf "%-45s %-10s %-12s %-10s %s\n" \
                "$session" \
                "$status" \
                "${bead:-(idle)}" \
                "${duration:--}" \
                "${workspace:--}"
        done
    fi
}

# Format output as JSON
_needle_list_json() {
    local workers=("$@")
    local wide="$NEEDLE_LIST_WIDE"

    if [[ ${#workers[@]} -eq 0 ]]; then
        echo "[]"
        return 0
    fi

    local json="["
    local first=true

    for worker in "${workers[@]}"; do
        IFS=$'\t' read -r session status bead duration workspace pid started agent <<< "$worker"

        # Escape values for JSON
        local session_escaped bead_escaped workspace_escaped agent_escaped
        session_escaped=$(_needle_json_escape "$session")
        bead_escaped=$(_needle_json_escape "${bead:-}")
        workspace_escaped=$(_needle_json_escape "${workspace:-}")
        agent_escaped=$(_needle_json_escape "${agent:-}")

        if [[ "$first" == "true" ]]; then
            first=false
        else
            json+=","
        fi

        if [[ "$wide" == "true" ]]; then
            json+=$(cat <<ENTRY
{
    "session": "$session_escaped",
    "status": "$status",
    "current_bead": $(_needle_json_nullable "${bead:-}"),
    "duration": "${duration:-}",
    "workspace": $(_needle_json_nullable "${workspace:-}"),
    "pid": ${pid:-null},
    "started": $(_needle_json_nullable "${started:-}"),
    "agent": $(_needle_json_nullable "${agent:-}")
}
ENTRY
)
        else
            json+=$(cat <<ENTRY
{
    "session": "$session_escaped",
    "status": "$status",
    "current_bead": $(_needle_json_nullable "${bead:-}"),
    "duration": "${duration:-}",
    "workspace": $(_needle_json_nullable "${workspace:-}")
}
ENTRY
)
        fi
    done

    json+="]"
    echo "$json"
}

# Main list command entry point
_needle_list() {
    local show_all=false
    local json_output=false
    local wide=false
    local quiet=false
    local filter_runner=""
    local filter_provider=""
    local filter_model=""
    local filter_workspace=""

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -a|--all)
                show_all=true
                shift
                ;;
            -j|--json)
                json_output=true
                shift
                ;;
            -w|--wide)
                wide=true
                shift
                ;;
            -q|--quiet)
                quiet=true
                shift
                ;;
            --runner)
                if [[ -z "${2:-}" ]] || [[ "$2" == -* ]]; then
                    _needle_error "Option --runner requires a value"
                    exit $NEEDLE_EXIT_USAGE
                fi
                filter_runner="$2"
                shift 2
                ;;
            --provider)
                if [[ -z "${2:-}" ]] || [[ "$2" == -* ]]; then
                    _needle_error "Option --provider requires a value"
                    exit $NEEDLE_EXIT_USAGE
                fi
                filter_provider="$2"
                shift 2
                ;;
            --model)
                if [[ -z "${2:-}" ]] || [[ "$2" == -* ]]; then
                    _needle_error "Option --model requires a value"
                    exit $NEEDLE_EXIT_USAGE
                fi
                filter_model="$2"
                shift 2
                ;;
            --workspace)
                if [[ -z "${2:-}" ]] || [[ "$2" == -* ]]; then
                    _needle_error "Option --workspace requires a value"
                    exit $NEEDLE_EXIT_USAGE
                fi
                filter_workspace="$2"
                shift 2
                ;;
            -h|--help)
                _needle_list_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            -*)
                _needle_error "Unknown option: $1"
                _needle_list_help
                exit $NEEDLE_EXIT_USAGE
                ;;
            *)
                _needle_error "Unexpected argument: $1"
                _needle_list_help
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    # Export wide flag for table/json formatters
    NEEDLE_LIST_WIDE="$wide"

    # Get tmux sessions
    local sessions
    sessions=$(_needle_list_sessions)

    # If --all, also get workers from registry that might be stopped
    local registry_workers=""
    if $show_all && [[ -f "$NEEDLE_WORKERS_FILE" ]]; then
        registry_workers=$(_needle_list_workers 2>/dev/null)
    fi

    # Build worker list
    local workers=()
    local processed_sessions=()

    # Process active tmux sessions
    while IFS= read -r session; do
        [[ -z "$session" ]] && continue

        # Parse session name components
        local runner="" provider="" model="" identifier=""
        if _needle_parse_session_name "$session"; then
            runner="$NEEDLE_SESSION_RUNNER"
            provider="$NEEDLE_SESSION_PROVIDER"
            model="$NEEDLE_SESSION_MODEL"
            identifier="$NEEDLE_SESSION_IDENTIFIER"
        fi

        # Apply filters
        [[ -n "$filter_runner" && "$runner" != "$filter_runner" ]] && continue
        [[ -n "$filter_provider" && "$provider" != "$filter_provider" ]] && continue
        [[ -n "$filter_model" && "$model" != "$filter_model" ]] && continue

        # Get heartbeat info
        local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
        local heartbeat_file="$heartbeat_dir/${session}.json"
        local status="unknown"
        local current_bead=""
        local bead_duration=""
        local workspace=""
        local pid=""
        local started=""
        local agent=""

        if [[ -f "$heartbeat_file" ]]; then
            status=$(jq -r '.status // "unknown"' "$heartbeat_file" 2>/dev/null)
            current_bead=$(jq -r '.current_bead // ""' "$heartbeat_file" 2>/dev/null)
            local bead_started
            bead_started=$(jq -r '.bead_started // ""' "$heartbeat_file" 2>/dev/null)

            if [[ -n "$bead_started" && "$bead_started" != "null" ]]; then
                bead_duration=$(_needle_list_duration "$bead_started")
            fi

            workspace=$(jq -r '.workspace // ""' "$heartbeat_file" 2>/dev/null)
            pid=$(jq -r '.pid // ""' "$heartbeat_file" 2>/dev/null)
            started=$(jq -r '.started // ""' "$heartbeat_file" 2>/dev/null)
            agent=$(jq -r '.agent // ""' "$heartbeat_file" 2>/dev/null)
        fi

        # Apply workspace filter
        [[ -n "$filter_workspace" && "$workspace" != *"$filter_workspace"* ]] && continue

        # Track processed sessions
        processed_sessions+=("$session")

        # Quiet mode: just output session names
        if $quiet; then
            echo "$session"
            continue
        fi

        # Build worker info (tab-separated for easy parsing)
        if $wide; then
            workers+=("$session"$'\t'"$status"$'\t'"$current_bead"$'\t'"$bead_duration"$'\t'"$workspace"$'\t'"$pid"$'\t'"$started"$'\t'"$agent")
        else
            workers+=("$session"$'\t'"$status"$'\t'"$current_bead"$'\t'"$bead_duration"$'\t'"$workspace")
        fi

    done <<< "$sessions"

    # Process registry workers (for --all) that aren't in tmux sessions
    if $show_all && [[ -n "$registry_workers" ]]; then
        while IFS= read -r reg_session; do
            [[ -z "$reg_session" ]] && continue

            # Skip if already processed (still running in tmux)
            local already_processed=false
            for proc in "${processed_sessions[@]}"; do
                if [[ "$proc" == "$reg_session" ]]; then
                    already_processed=true
                    break
                fi
            done
            $already_processed && continue

            # Get worker info from registry
            local reg_info
            reg_info=$(_needle_get_worker "$reg_session" 2>/dev/null)

            if [[ -n "$reg_info" && "$reg_info" != "{}" ]]; then
                local runner provider model identifier reg_workspace reg_pid reg_started

                runner=$(echo "$reg_info" | jq -r '.runner // ""')
                provider=$(echo "$reg_info" | jq -r '.provider // ""')
                model=$(echo "$reg_info" | jq -r '.model // ""')
                identifier=$(echo "$reg_info" | jq -r '.identifier // ""')
                reg_workspace=$(echo "$reg_info" | jq -r '.workspace // ""')
                reg_pid=$(echo "$reg_info" | jq -r '.pid // ""')
                reg_started=$(echo "$reg_info" | jq -r '.started // ""')

                # Apply filters
                [[ -n "$filter_runner" && "$runner" != "$filter_runner" ]] && continue
                [[ -n "$filter_provider" && "$provider" != "$filter_provider" ]] && continue
                [[ -n "$filter_model" && "$model" != "$filter_model" ]] && continue
                [[ -n "$filter_workspace" && "$reg_workspace" != *"$filter_workspace"* ]] && continue

                # Quiet mode: just output session names
                if $quiet; then
                    echo "$reg_session (stopped)"
                    continue
                fi

                # Worker is in registry but not in tmux = stopped
                local status="stopped"
                local current_bead=""
                local bead_duration="-"

                # Check for heartbeat file
                local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
                local heartbeat_file="$heartbeat_dir/${reg_session}.json"

                if [[ -f "$heartbeat_file" ]]; then
                    status=$(jq -r '.status // "stopped"' "$heartbeat_file" 2>/dev/null)
                    # If status is executing/draining, the worker likely crashed
                    if [[ "$status" == "executing" || "$status" == "draining" ]]; then
                        status="crashed"
                    fi
                fi

                if $wide; then
                    workers+=("$reg_session"$'\t'"$status"$'\t'"$current_bead"$'\t'"$bead_duration"$'\t'"$reg_workspace"$'\t'"$reg_pid"$'\t'"$reg_started"$'\t'"")
                else
                    workers+=("$reg_session"$'\t'"$status"$'\t'"$current_bead"$'\t'"$bead_duration"$'\t'"$reg_workspace")
                fi
            fi
        done < <(echo "$registry_workers" | jq -r '.workers[].session' 2>/dev/null)
    fi

    # Handle empty state
    if [[ ${#workers[@]} -eq 0 ]] && ! $quiet; then
        if $json_output; then
            echo "[]"
        else
            _needle_info "No workers running"
        fi
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # Quiet mode already outputted names
    if $quiet; then
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # Output
    if $json_output; then
        _needle_list_json "${workers[@]}"
    else
        _needle_list_table "${workers[@]}"
    fi

    exit $NEEDLE_EXIT_SUCCESS
}
