#!/usr/bin/env bash
# NEEDLE CLI Stop Subcommand
# Stop running workers with various shutdown modes

# Shutdown signal directory
NEEDLE_SHUTDOWN_DIR="${NEEDLE_SHUTDOWN_DIR:-$NEEDLE_HOME/$NEEDLE_STATE_DIR/shutdown}"

_needle_stop_help() {
    _needle_print "Stop running NEEDLE worker(s)

Stops one or more workers. By default, uses graceful shutdown which
waits for the current bead to complete before exiting.

USAGE:
    needle stop [WORKERS...] [OPTIONS]

ARGUMENTS:
    [WORKERS...]    Worker identifiers to stop (e.g., \"alpha bravo\")
                    Use --all to stop all workers

OPTIONS:
    -a, --all                Stop all running workers
    -g, --graceful           Wait for current bead to complete [default]
    -i, --immediate          Stop immediately, release current bead
    -f, --force              Kill process without cleanup (last resort)

    --timeout <SECONDS>      Graceful shutdown timeout [default: 300]

    -h, --help               Print help information

SHUTDOWN MODES:
    graceful    Finish current bead, then exit cleanly
    immediate   Release current bead to queue, exit now
    force       SIGKILL the process (may orphan bead)

EXAMPLES:
    # Stop worker alpha gracefully
    needle stop alpha

    # Stop all workers gracefully
    needle stop --all

    # Stop immediately (release bead back to queue)
    needle stop alpha --immediate

    # Stop all with custom timeout
    needle stop --all --timeout=60

    # Force kill (use only if stuck)
    needle stop alpha --force

NOTES:
    - Graceful shutdown waits for current bead to complete
    - If timeout is reached, bead is released and worker exits
    - Force stop may leave beads in inconsistent state
"
}

# Main stop command entry point
_needle_stop() {
    local workers=()
    local all=false
    local mode="graceful"  # graceful | immediate | force
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
            -f|--force)
                mode="force"
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
                _needle_stop_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            -*)
                _needle_error "Unknown option: $1"
                _needle_stop_help
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
        _needle_error "No workers to stop"
        exit $NEEDLE_EXIT_ERROR
    fi

    # Ensure shutdown directory exists
    mkdir -p "$NEEDLE_SHUTDOWN_DIR" 2>/dev/null

    # Stop each session
    local stopped=0
    local failed=0

    for session in "${sessions[@]}"; do
        _needle_verbose "Stopping $session with mode: $mode"

        case "$mode" in
            graceful)
                if _needle_stop_graceful "$session" "$timeout"; then
                    ((stopped++))
                else
                    ((failed++))
                fi
                ;;
            immediate)
                if _needle_stop_immediate "$session"; then
                    ((stopped++))
                else
                    ((failed++))
                fi
                ;;
            force)
                if _needle_stop_force "$session"; then
                    ((stopped++))
                else
                    ((failed++))
                fi
                ;;
        esac
    done

    # Summary
    _needle_print ""
    if [[ $failed -gt 0 ]]; then
        _needle_warn "Stopped $stopped worker(s), $failed failed"
        exit $NEEDLE_EXIT_ERROR
    else
        _needle_success "Stopped $stopped worker(s)"
        exit $NEEDLE_EXIT_SUCCESS
    fi
}

# Graceful stop - signal and wait for clean exit
_needle_stop_graceful() {
    local session="$1"
    local timeout="$2"

    if ! _needle_session_exists "$session"; then
        _needle_warn "Session does not exist: $session"
        return 1
    fi

    _needle_info "Gracefully stopping $session..."

    # Create shutdown signal file
    # Extract identifier from session name (last component)
    local identifier="${session##*-}"
    local signal_file="$NEEDLE_SHUTDOWN_DIR/shutdown-$identifier"

    # Also create signal file with full session name for uniqueness
    local full_signal_file="$NEEDLE_SHUTDOWN_DIR/shutdown-$session"

    # Create signal files
    touch "$signal_file" 2>/dev/null || true
    touch "$full_signal_file" 2>/dev/null || true

    _needle_verbose "Created shutdown signal: $signal_file"

    # Wait for clean exit
    local waited=0
    local progress_interval=30
    local next_progress=$progress_interval

    while _needle_session_exists "$session" && ((waited < timeout)); do
        sleep 1
        ((waited++))

        # Show progress periodically
        if ((waited >= next_progress)); then
            _needle_verbose "Waiting for $session to complete (${waited}s/${timeout}s)..."
            next_progress=$((next_progress + progress_interval))
        fi
    done

    # Check if session still exists
    if _needle_session_exists "$session"; then
        _needle_warn "Timeout reached for $session after ${timeout}s"
        _needle_info "Killing session..."
        _needle_kill_session "$session" 2>/dev/null || true
    fi

    # Cleanup signal files
    rm -f "$signal_file" 2>/dev/null || true
    rm -f "$full_signal_file" 2>/dev/null || true

    # Cleanup heartbeat file
    _needle_stop_cleanup_heartbeat "$session"

    # Unregister worker from state
    _needle_unregister_worker "$session" 2>/dev/null || true

    _needle_success "Stopped $session"
    return 0
}

# Immediate stop - release bead and kill
_needle_stop_immediate() {
    local session="$1"

    if ! _needle_session_exists "$session"; then
        _needle_warn "Session does not exist: $session"
        return 1
    fi

    _needle_info "Immediately stopping $session..."

    # Get current bead from heartbeat
    local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
    local heartbeat_file="$heartbeat_dir/${session}.json"

    if [[ -f "$heartbeat_file" ]]; then
        local current_bead
        current_bead=$(jq -r '.current_bead // ""' "$heartbeat_file" 2>/dev/null)

        if [[ -n "$current_bead" && "$current_bead" != "null" && "$current_bead" != "" ]]; then
            _needle_info "Releasing bead: $current_bead"

            # Use br CLI to release the bead if available
            if command -v br &>/dev/null; then
                br update "$current_bead" --release --reason "immediate_stop" 2>/dev/null || \
                    _needle_warn "Failed to release bead $current_bead"
            else
                _needle_verbose "br CLI not available, skipping bead release"
            fi
        fi
    fi

    # Kill the session
    _needle_kill_session "$session" 2>/dev/null || true

    # Cleanup heartbeat file
    _needle_stop_cleanup_heartbeat "$session"

    # Unregister worker from state
    _needle_unregister_worker "$session" 2>/dev/null || true

    _needle_success "Stopped $session"
    return 0
}

# Force stop - kill without cleanup
_needle_stop_force() {
    local session="$1"

    _needle_warn "Force killing $session..."

    # Kill without checking existence first
    tmux kill-session -t "$session" 2>/dev/null || true

    # Still cleanup state files
    _needle_stop_cleanup_heartbeat "$session"

    # Unregister worker from state
    _needle_unregister_worker "$session" 2>/dev/null || true

    _needle_success "Killed $session"
    return 0
}

# Cleanup heartbeat file for a session
_needle_stop_cleanup_heartbeat() {
    local session="$1"

    local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
    local heartbeat_file="$heartbeat_dir/${session}.json"

    if [[ -f "$heartbeat_file" ]]; then
        rm -f "$heartbeat_file" 2>/dev/null
        _needle_verbose "Removed heartbeat: $heartbeat_file"
    fi

    # Also cleanup shutdown signal files
    local identifier="${session##*-}"
    rm -f "$NEEDLE_SHUTDOWN_DIR/shutdown-$identifier" 2>/dev/null || true
    rm -f "$NEEDLE_SHUTDOWN_DIR/shutdown-$session" 2>/dev/null || true
}
