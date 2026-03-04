#!/usr/bin/env bash
# NEEDLE CLI Restart Subcommand
# Restart workers with graceful or immediate modes

# Use shutdown directory from stop.sh if available
NEEDLE_SHUTDOWN_DIR="${NEEDLE_SHUTDOWN_DIR:-$NEEDLE_HOME/$NEEDLE_STATE_DIR/shutdown}"

_needle_restart_help() {
    _needle_print "Restart running workers

Gracefully or immediately restart NEEDLE workers. By default, workers
are given time to complete their current bead before restarting.

USAGE:
    needle restart [WORKERS...] [OPTIONS]

ARGUMENTS:
    [WORKERS...]    Worker identifiers to restart (session names or identifiers)

OPTIONS:
    -a, --all         Restart all running workers
    -g, --graceful    Wait for current bead to complete [default]
    -i, --immediate   Restart immediately, release current bead
    --timeout <SECS>  Graceful shutdown timeout [default: 300]
    -h, --help        Show this help message

RESTART MODES:
    graceful   Signal worker to stop after completing current bead.
               Worker will restart once current work is done.
               Preserves worker configuration.

    immediate  Signal worker to stop now. Any current bead is released
               back to the queue for another worker to pick up.
               Worker is restarted immediately after.

EXAMPLES:
    # Restart specific worker gracefully
    needle restart needle-claude-anthropic-sonnet-alpha

    # Restart worker by short identifier
    needle restart alpha

    # Restart all workers
    needle restart --all

    # Restart immediately (releases current bead)
    needle restart --immediate needle-claude-anthropic-sonnet-alpha

    # Graceful restart with custom timeout
    needle restart --timeout 600 needle-claude-anthropic-sonnet-alpha
"
}

# Wait for a worker to become idle (no current bead)
# Arguments:
#   $1 - Session name
#   $2 - Timeout in seconds
# Returns: 0 if worker became idle, 1 if timeout
_needle_wait_for_idle() {
    local session="$1"
    local timeout="$2"

    local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
    local heartbeat_file="$heartbeat_dir/${session}.json"

    local waited=0
    local check_interval=5

    while ((waited < timeout)); do
        # Check if session still exists
        if ! _needle_session_exists "$session"; then
            _needle_verbose "Session $session no longer exists"
            return 0
        fi

        # Check heartbeat for current bead
        if [[ -f "$heartbeat_file" ]]; then
            local current_bead
            current_bead=$(jq -r '.current_bead // ""' "$heartbeat_file" 2>/dev/null)

            if [[ -z "$current_bead" || "$current_bead" == "null" || "$current_bead" == "" ]]; then
                _needle_verbose "Worker $session is now idle"
                return 0
            fi
        else
            # No heartbeat file means worker is likely idle or not running
            return 0
        fi

        sleep "$check_interval"
        ((waited += check_interval))
        _needle_verbose "Waiting for $session to become idle (${waited}s/${timeout}s)..."
    done

    _needle_warn "Timeout waiting for $session to become idle"
    return 1
}

# Get worker configuration from registry for respawning
# Arguments:
#   $1 - Session name
# Returns: JSON object with worker config or empty object
_needle_get_worker_config() {
    local session="$1"

    # Get from registry
    local worker_info
    worker_info=$(_needle_get_worker "$session" 2>/dev/null)

    if [[ -n "$worker_info" && "$worker_info" != "{}" ]]; then
        echo "$worker_info"
        return 0
    fi

    # Try to parse from session name as fallback
    if _needle_parse_session_name "$session"; then
        jq -n \
            --arg s "$session" \
            --arg r "$NEEDLE_SESSION_RUNNER" \
            --arg p "$NEEDLE_SESSION_PROVIDER" \
            --arg m "$NEEDLE_SESSION_MODEL" \
            --arg i "$NEEDLE_SESSION_IDENTIFIER" \
            '{
                session: $s,
                runner: $r,
                provider: $p,
                model: $m,
                identifier: $i,
                workspace: "",
                agent: ($r + "-" + $p + "-" + $m)
            }'
        return 0
    fi

    echo "{}"
    return 1
}

# Respawn a worker with the same configuration
# Arguments:
#   $1 - Session name
#   $2 - Worker config JSON
# Returns: 0 on success, 1 on failure
_needle_respawn_worker() {
    local old_session="$1"
    local config_json="$2"

    # Extract config values
    local runner provider model identifier workspace agent

    runner=$(echo "$config_json" | jq -r '.runner // ""')
    provider=$(echo "$config_json" | jq -r '.provider // ""')
    model=$(echo "$config_json" | jq -r '.model // ""')
    identifier=$(echo "$config_json" | jq -r '.identifier // ""')
    workspace=$(echo "$config_json" | jq -r '.workspace // ""')
    agent=$(echo "$config_json" | jq -r '.agent // ""')

    # Validate we have required info
    if [[ -z "$runner" || -z "$provider" || -z "$model" ]]; then
        _needle_error "Cannot respawn worker: missing configuration"
        return 1
    fi

    # Generate new identifier (the old one should be free now)
    local new_identifier
    new_identifier=$(_needle_next_identifier "$runner" "$provider" "$model")

    # Generate new session name
    local new_session
    new_session=$(_needle_generate_session_name "" "$runner" "$provider" "$model" "$new_identifier")

    _needle_verbose "Respawning worker as: $new_session"
    _needle_verbose "  Runner: $runner, Provider: $provider, Model: $model"
    _needle_verbose "  Workspace: ${workspace:-<default>}"

    # Build the command to start the worker
    local cmd_args=("$NEEDLE_ROOT_DIR/bin/needle" "run")

    if [[ -n "$workspace" ]]; then
        cmd_args+=("--workspace" "$workspace")
    fi

    if [[ -n "$agent" ]]; then
        cmd_args+=("--agent" "$agent")
    fi

    # Create the new session
    if _needle_create_session "$new_session" "${cmd_args[*]}"; then
        NEEDLE_RESTARTED_SESSION="$new_session"
        return 0
    else
        _needle_error "Failed to create new session: $new_session"
        return 1
    fi
}

# Main restart command entry point
_needle_restart() {
    local workers=()
    local all=false
    local mode="graceful"  # graceful | immediate
    local timeout=300

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -a|--all)
                all=true
                shift
                ;;
            -g|--graceful)
                mode="graceful"
                shift
                ;;
            -i|--immediate)
                mode="immediate"
                shift
                ;;
            --timeout)
                if [[ -z "${2:-}" ]] || [[ "$2" == -* ]]; then
                    _needle_error "Option --timeout requires a value"
                    exit $NEEDLE_EXIT_USAGE
                fi
                timeout="$2"
                shift 2
                ;;
            -h|--help)
                _needle_restart_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            -*)
                _needle_error "Unknown option: $1"
                _needle_restart_help
                exit $NEEDLE_EXIT_USAGE
                ;;
            *)
                workers+=("$1")
                shift
                ;;
        esac
    done

    # Validate arguments
    if [[ "$all" == "true" ]] && [[ ${#workers[@]} -gt 0 ]]; then
        _needle_error "Cannot specify both --all and specific workers"
        exit $NEEDLE_EXIT_USAGE
    fi

    if [[ "$all" == "false" ]] && [[ ${#workers[@]} -eq 0 ]]; then
        _needle_error "No workers specified. Use --all or provide worker identifiers"
        _needle_info "Run 'needle list' to see running workers"
        exit $NEEDLE_EXIT_USAGE
    fi

    # Get target sessions
    local sessions=()

    if $all; then
        # Get all needle sessions
        while IFS= read -r session; do
            [[ -n "$session" ]] && sessions+=("$session")
        done < <(_needle_list_sessions)

        if [[ ${#sessions[@]} -eq 0 ]]; then
            _needle_info "No running workers found"
            exit $NEEDLE_EXIT_SUCCESS
        fi
    else
        # Resolve worker identifiers to sessions
        for worker in "${workers[@]}"; do
            local session=""

            # Check if it's already a full session name
            if [[ "$worker" == needle-* ]]; then
                if _needle_session_exists "$worker"; then
                    session="$worker"
                fi
            else
                # Try to find session by identifier
                session=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | \
                    grep -E "^needle-.*-${worker}$" | head -1)

                # Also try matching anywhere in the name
                if [[ -z "$session" ]]; then
                    session=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | \
                        grep -E "^needle-.*${worker}" | head -1)
                fi
            fi

            if [[ -n "$session" ]]; then
                sessions+=("$session")
            else
                _needle_warn "Worker not found: $worker"
            fi
        done
    fi

    if [[ ${#sessions[@]} -eq 0 ]]; then
        _needle_error "No workers to restart"
        exit $NEEDLE_EXIT_ERROR
    fi

    # Ensure shutdown directory exists
    mkdir -p "$NEEDLE_SHUTDOWN_DIR" 2>/dev/null

    # Restart each session
    local restarted=0
    local failed=0

    for session in "${sessions[@]}"; do
        _needle_verbose "Restarting $session with mode: $mode"

        # Get worker configuration before stopping
        local worker_config
        worker_config=$(_needle_get_worker_config "$session")

        if [[ "$worker_config" == "{}" ]]; then
            _needle_warn "Could not get configuration for $session, skipping"
            ((failed++))
            continue
        fi

        local success=false

        case "$mode" in
            graceful)
                # For graceful restart, wait for worker to become idle first
                _needle_info "Gracefully restarting $session..."

                # Create drain signal
                local identifier="${session##*-}"
                local signal_file="$NEEDLE_SHUTDOWN_DIR/drain-$identifier"
                touch "$signal_file" 2>/dev/null || true

                _needle_verbose "Signaling $session to drain..."

                # Wait for worker to become idle
                if _needle_wait_for_idle "$session" "$timeout"; then
                    # Worker is idle, now stop and restart
                    if _needle_stop_graceful "$session" "30"; then
                        # Small delay to ensure cleanup
                        sleep 1

                        # Respawn with saved config
                        if _needle_respawn_worker "$session" "$worker_config"; then
                            _needle_success "Restarted $session -> $NEEDLE_RESTARTED_SESSION"
                            success=true
                        fi
                    fi
                else
                    _needle_warn "Timeout waiting for $session to become idle"
                    # Fall through to stop anyway
                    if _needle_stop_graceful "$session" "30"; then
                        sleep 1
                        if _needle_respawn_worker "$session" "$worker_config"; then
                            _needle_success "Restarted $session -> $NEEDLE_RESTARTED_SESSION"
                            success=true
                        fi
                    fi
                fi

                # Cleanup drain signal
                rm -f "$signal_file" 2>/dev/null || true
                ;;

            immediate)
                _needle_info "Immediately restarting $session..."

                # Stop immediately (releases bead)
                if _needle_stop_immediate "$session"; then
                    # Small delay to ensure cleanup
                    sleep 1

                    # Respawn with saved config
                    if _needle_respawn_worker "$session" "$worker_config"; then
                        _needle_success "Restarted $session -> $NEEDLE_RESTARTED_SESSION"
                        success=true
                    fi
                fi
                ;;
        esac

        if $success; then
            ((restarted++))
        else
            ((failed++))
        fi
    done

    # Summary
    _needle_print ""
    if [[ $failed -gt 0 ]]; then
        _needle_warn "Restarted $restarted worker(s), $failed failed"
        exit $NEEDLE_EXIT_ERROR
    else
        _needle_success "Restarted $restarted worker(s)"
        exit $NEEDLE_EXIT_SUCCESS
    fi
}
