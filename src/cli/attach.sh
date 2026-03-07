#!/usr/bin/env bash
# NEEDLE CLI Attach Subcommand
# Attach to worker tmux sessions

_needle_attach_help() {
    _needle_print "Attach to a worker's tmux session

Connects your terminal to a running worker's tmux session for
monitoring or debugging. Detach with Ctrl+B, D.

USAGE:
    needle attach [WORKER]

ARGUMENTS:
    [WORKER]    Worker identifier or full session name
                Can be: \"alpha\", \"bravo\", or full name like
                \"needle-claude-anthropic-sonnet-alpha\"
                [default: most recently started worker]

OPTIONS:
    -r, --read-only          Attach in read-only mode (view only)
    -l, --last               Attach to most recent worker

    -h, --help               Print help information

EXAMPLES:
    # Attach to worker alpha
    needle attach alpha

    # Attach to most recent worker
    needle attach --last

    # Attach read-only (can't type, just watch)
    needle attach alpha --read-only

    # Attach by full session name
    needle attach needle-claude-anthropic-sonnet-alpha

DETACHING:
    Press Ctrl+B, then D to detach from the session.
    The worker continues running in the background.

SEE ALSO:
    needle list    List available workers to attach to
    needle logs    View worker logs without attaching
"
}

_needle_attach() {
    local worker=""
    local read_only=false
    local last=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -r|--read-only)
                read_only=true
                shift
                ;;
            -l|--last)
                last=true
                shift
                ;;
            -h|--help)
                _needle_attach_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            -*)
                _needle_error "Unknown option: $1"
                _needle_attach_help
                exit $NEEDLE_EXIT_USAGE
                ;;
            *)
                worker="$1"
                shift
                ;;
        esac
    done

    # Check if tmux is available
    if ! _needle_tmux_available; then
        _needle_error "tmux is not available"
        _needle_info "Install tmux to use worker sessions"
        exit $NEEDLE_EXIT_RUNTIME
    fi

    # Get session name
    local session=""

    if [[ "$last" == "true" ]] || [[ -z "$worker" ]]; then
        # Get most recent session
        session=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep '^needle-' | tail -1 || true)

        if [[ -z "$session" ]] && [[ -n "$worker" ]]; then
            # --last was not set, but we fell through due to no worker
            # Try to find the worker
            :
        fi
    fi

    # If we don't have a session yet and have a worker spec, look it up
    if [[ -z "$session" ]] && [[ -n "$worker" ]]; then
        if [[ "$worker" == needle-* ]]; then
            # Full session name provided
            session="$worker"
        else
            # Try exact identifier match first (needle-*-*-*-$worker)
            session=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep "^needle-.*-${worker}$" | head -1 || true)

            if [[ -z "$session" ]]; then
                # Try looser match (worker anywhere in name)
                session=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep "needle-.*${worker}" | head -1 || true)
            fi
        fi
    fi

    # Verify session exists
    if [[ -z "$session" ]]; then
        if [[ -z "$worker" ]]; then
            _needle_error "No running workers found"
        else
            _needle_error "Worker not found: $worker"
        fi
        _needle_info "Run 'needle list' to see available workers"
        exit $NEEDLE_EXIT_RUNTIME
    fi

    if ! _needle_session_exists "$session"; then
        _needle_error "Session does not exist: $session"
        exit $NEEDLE_EXIT_RUNTIME
    fi

    # Show attach info
    _needle_info "Attaching to $session..."
    _needle_info "Detach with Ctrl+B, D"
    _needle_print ""

    # Attach to session
    _needle_attach_session "$session" "$read_only"
}
