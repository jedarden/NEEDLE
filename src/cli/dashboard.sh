#!/usr/bin/env bash
# NEEDLE CLI Dashboard Subcommand
# Start the FABRIC real-time dashboard server

# Dashboard server configuration
NEEDLE_DASHBOARD_DEFAULT_PORT=7842
NEEDLE_DASHBOARD_DEFAULT_BUFFER_SIZE=10000
NEEDLE_DASHBOARD_PID_FILE="${NEEDLE_HOME:-$HOME/.needle}/dashboard.pid"
NEEDLE_DASHBOARD_LOG_FILE="${NEEDLE_HOME:-$HOME/.needle}/logs/dashboard.log"

# Get configured dashboard port (config takes precedence over default)
_needle_dashboard_get_port() {
    if declare -f get_config &>/dev/null; then
        get_config "dashboard.port" "$NEEDLE_DASHBOARD_DEFAULT_PORT"
    else
        echo "$NEEDLE_DASHBOARD_DEFAULT_PORT"
    fi
}

_needle_dashboard_help() {
    _needle_print "Start the FABRIC real-time dashboard server

The dashboard receives events from NEEDLE workers via FABRIC forwarding
and displays them in a live web UI with SSE streaming.

USAGE:
    needle dashboard <command> [OPTIONS]

COMMANDS:
    start       Start the dashboard server in the background
    stop        Stop the dashboard server
    restart     Restart the dashboard server
    status      Check if the dashboard is running
    logs        View dashboard logs

OPTIONS:
    -p, --port <PORT>          Port to listen on (default: $NEEDLE_DASHBOARD_DEFAULT_PORT)
    -b, --buffer-size <N>      Event buffer size (default: $NEEDLE_DASHBOARD_DEFAULT_BUFFER_SIZE)
    -s, --seed-file <FILE>     JSONL file to seed event buffer from
    -f, --foreground           Run in foreground (don't daemonize)
    -o, --open                 Open dashboard in browser after starting
    -h, --help                 Print help information

ENDPOINTS (when running):
    http://localhost:<PORT>/           Dashboard UI
    http://localhost:<PORT>/stream     SSE event stream
    http://localhost:<PORT>/api/summary  Aggregate stats JSON
    http://localhost:<PORT>/ingest     Event ingestion endpoint

CONFIGURATION:
    Set FABRIC_ENDPOINT in your needle config to point to the dashboard:
    fabric:
      enabled: true
      endpoint: http://localhost:$NEEDLE_DASHBOARD_DEFAULT_PORT/ingest

EXAMPLES:
    # Start dashboard on default port
    needle dashboard start

    # Start on custom port and open browser
    needle dashboard start --port 3000 --open

    # Check if running
    needle dashboard status

    # Stop the dashboard
    needle dashboard stop
"
}

_needle_dashboard() {
    local command="${1:-help}"
    shift || true

    case "$command" in
        start)
            _needle_dashboard_start "$@"
            ;;
        stop)
            _needle_dashboard_stop "$@"
            ;;
        restart)
            _needle_dashboard_restart "$@"
            ;;
        status)
            _needle_dashboard_status "$@"
            ;;
        logs)
            _needle_dashboard_logs "$@"
            ;;
        help|-h|--help)
            _needle_dashboard_help
            exit $NEEDLE_EXIT_SUCCESS
            ;;
        *)
            _needle_error "Unknown dashboard command: $command"
            _needle_print ""
            _needle_dashboard_help
            exit $NEEDLE_EXIT_USAGE
            ;;
    esac
}

_needle_dashboard_start() {
    local port
    port="$(_needle_dashboard_get_port)"
    local buffer_size="$NEEDLE_DASHBOARD_DEFAULT_BUFFER_SIZE"
    local seed_file=""
    local foreground=false
    local open_browser=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -p|--port)
                port="$2"
                shift 2
                ;;
            -b|--buffer-size)
                buffer_size="$2"
                shift 2
                ;;
            -s|--seed-file)
                seed_file="$2"
                shift 2
                ;;
            -f|--foreground)
                foreground=true
                shift
                ;;
            -o|--open)
                open_browser=true
                shift
                ;;
            -h|--help)
                _needle_dashboard_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                _needle_error "Unknown option: $1"
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    # Find the dashboard server script
    local server_script="$NEEDLE_ROOT_DIR/src/dashboard/server.py"
    if [[ ! -f "$server_script" ]]; then
        _needle_error "Dashboard server not found: $server_script"
        exit $NEEDLE_EXIT_CONFIG
    fi

    # Check if already running
    if _needle_dashboard_is_running; then
        _needle_warn "Dashboard is already running"
        _needle_dashboard_status
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # Ensure log directory exists
    mkdir -p "$(dirname "$NEEDLE_DASHBOARD_LOG_FILE")" 2>/dev/null || true

    # Build command args array (avoids eval and nohup-eval issues)
    local cmd_args=(python3 "$server_script" --port "$port" --buffer-size "$buffer_size")
    if [[ -n "$seed_file" ]]; then
        cmd_args+=(--seed-file "$seed_file")
    fi

    if [[ "$foreground" == "true" ]]; then
        _needle_info "Starting FABRIC dashboard on port $port..."
        "${cmd_args[@]}"
    else
        # Run in background
        _needle_info "Starting FABRIC dashboard on port $port (background)..."

        # Start server and capture PID
        nohup "${cmd_args[@]}" >> "$NEEDLE_DASHBOARD_LOG_FILE" 2>&1 &
        local pid=$!

        # Wait briefly to confirm startup
        sleep 1

        if kill -0 "$pid" 2>/dev/null; then
            echo "$pid" > "$NEEDLE_DASHBOARD_PID_FILE"
            _needle_success "Dashboard started (PID: $pid)"
            _needle_print ""
            _needle_print "  Dashboard:  http://localhost:$port/"
            _needle_print "  SSE stream: http://localhost:$port/stream"
            _needle_print "  API:        http://localhost:$port/api/summary"
            _needle_print ""
            _needle_print "  Logs: $NEEDLE_DASHBOARD_LOG_FILE"
            _needle_print "  Stop: needle dashboard stop"

            # Open browser if requested
            if [[ "$open_browser" == "true" ]]; then
                if command -v xdg-open &>/dev/null; then
                    xdg-open "http://localhost:$port/" 2>/dev/null &
                elif command -v open &>/dev/null; then
                    open "http://localhost:$port/" 2>/dev/null &
                fi
            fi
        else
            _needle_error "Failed to start dashboard server"
            _needle_info "Check logs: $NEEDLE_DASHBOARD_LOG_FILE"
            exit $NEEDLE_EXIT_FAILURE
        fi
    fi

    exit $NEEDLE_EXIT_SUCCESS
}

_needle_dashboard_do_stop() {
    # Internal helper: stops the dashboard without calling exit.
    if ! _needle_dashboard_is_running; then
        _needle_info "Dashboard is not running"
        return 0
    fi

    local pid
    pid=$(cat "$NEEDLE_DASHBOARD_PID_FILE" 2>/dev/null)

    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        _needle_info "Stopping dashboard (PID: $pid)..."
        kill "$pid" 2>/dev/null

        # Wait for graceful shutdown
        local waited=0
        while kill -0 "$pid" 2>/dev/null && [[ $waited -lt 10 ]]; do
            sleep 1
            ((waited++))
        done

        # Force kill if still running
        if kill -0 "$pid" 2>/dev/null; then
            _needle_warn "Dashboard didn't stop gracefully, killing..."
            kill -9 "$pid" 2>/dev/null
        fi

        rm -f "$NEEDLE_DASHBOARD_PID_FILE"
        _needle_success "Dashboard stopped"
    else
        rm -f "$NEEDLE_DASHBOARD_PID_FILE"
        _needle_info "Dashboard was not running (stale PID file removed)"
    fi
}

_needle_dashboard_stop() {
    _needle_dashboard_do_stop
    exit $NEEDLE_EXIT_SUCCESS
}

_needle_dashboard_restart() {
    _needle_info "Restarting dashboard..."
    _needle_dashboard_do_stop
    sleep 1
    _needle_dashboard_start "$@"
}

_needle_dashboard_status() {
    if _needle_dashboard_is_running; then
        local pid
        pid=$(cat "$NEEDLE_DASHBOARD_PID_FILE" 2>/dev/null)

        _needle_success "Dashboard is running (PID: $pid)"

        # Try to get port from running process
        local port=""
        if command -v lsof &>/dev/null; then
            port=$(lsof -i -P -n 2>/dev/null | grep "LISTEN" | grep "$pid" | grep -oE ':[0-9]+' | head -1 | tr -d ':')
        fi

        if [[ -n "$port" ]]; then
            _needle_print "  URL: http://localhost:$port/"
        fi

        # Check health endpoint
        if [[ -n "$port" ]]; then
            local health
            health=$(curl -s --max-time 2 "http://localhost:$port/health" 2>/dev/null || echo "")
            if [[ -n "$health" ]]; then
                _needle_print "  Health: $health"
            fi
        fi

        exit $NEEDLE_EXIT_SUCCESS
    else
        _needle_info "Dashboard is not running"
        exit $NEEDLE_EXIT_FAILURE
    fi
}

_needle_dashboard_logs() {
    local follow=false
    local lines=50

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
            *)
                shift
                ;;
        esac
    done

    if [[ ! -f "$NEEDLE_DASHBOARD_LOG_FILE" ]]; then
        _needle_info "No log file found: $NEEDLE_DASHBOARD_LOG_FILE"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    if [[ "$follow" == "true" ]]; then
        tail -f "$NEEDLE_DASHBOARD_LOG_FILE"
    else
        tail -n "$lines" "$NEEDLE_DASHBOARD_LOG_FILE"
    fi
}

_needle_dashboard_is_running() {
    [[ -f "$NEEDLE_DASHBOARD_PID_FILE" ]] || return 1

    local pid
    pid=$(cat "$NEEDLE_DASHBOARD_PID_FILE" 2>/dev/null)

    [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}
