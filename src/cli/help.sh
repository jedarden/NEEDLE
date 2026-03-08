#!/usr/bin/env bash
# NEEDLE CLI Help Subcommand
# Display help and documentation

_needle_help() {
    local topic="${1:-}"

    if [[ -z "$topic" ]]; then
        _needle_help_main
        return
    fi

    # Route to specific subcommand help
    case "$topic" in
        init)
            _needle_init_help
            ;;
        run)
            _needle_run_help
            ;;
        list)
            _needle_list_help
            ;;
        attach)
            _needle_attach_help
            ;;
        stop)
            _needle_stop_help
            ;;
        logs)
            _needle_logs_help
            ;;
        status)
            _needle_status_help
            ;;
        setup)
            _needle_setup_help
            ;;
        upgrade)
            _needle_upgrade_help
            ;;
        rollback)
            _needle_rollback_help
            ;;
        version)
            _needle_version_help
            ;;
        agents)
            _needle_agents_help
            ;;
        test-agent)
            _needle_test_agent_help
            ;;
        heartbeat)
            _needle_heartbeat_help
            ;;
        config)
            _needle_config_help
            ;;
        pulse)
            _needle_pulse_help
            ;;
        help)
            _needle_print "Display help for NEEDLE commands"
            _needle_print ""
            _needle_print "USAGE:"
            _needle_print "    needle help [COMMAND]"
            _needle_print ""
            _needle_print "Display help for a specific command."
            ;;
        *)
            _needle_error "Unknown command: $topic"
            _needle_help_main
            ;;
    esac
}

_needle_help_main() {
    cat << 'EOF'
NEEDLE - Navigates Every Enqueued Deliverable, Logs Effort

A universal wrapper for headless coding CLI agents that processes
beads (tasks) from a queue with automatic session management.

USAGE:
    needle <COMMAND> [OPTIONS]
    needle [OPTIONS]

COMMANDS:
    init        Interactive first-time setup and onboarding
    run         Start a worker to process beads
    list        List running workers
    attach      Attach to a worker's tmux session
    stop        Stop running worker(s)
    logs        View or tail worker logs
    status      Show worker health and statistics

    setup       Check and install dependencies
    upgrade     Check for and install updates
    rollback    Rollback to a previous version
    version     Show version information

    agents      List and manage agent adapters
    test-agent  Test an agent adapter configuration

    heartbeat   Manage worker heartbeat and recovery
    config      View or edit configuration
    pulse       Run codebase health scan manually

OPTIONS:
    -h, --help       Print help information
    -V, --version    Print version information
    -v, --verbose    Enable verbose output
    -q, --quiet      Suppress non-error output
    --no-color       Disable colored output

QUICK START:
    # First time setup (runs automatically if unconfigured)
    needle init

    # Start a worker
    needle run --workspace=/path/to/project --agent=claude-anthropic-sonnet

    # List running workers
    needle list

    # Attach to a worker session
    needle attach alpha

CONFIGURATION:
    Global config:    ~/.needle/config.yaml
    Workspace config: .needle.yaml (in workspace root)
    Logs:             ~/.needle/logs/

DOCUMENTATION:
    Full docs:  https://github.com/user/needle#readme
    Issues:     https://github.com/user/needle/issues

Use "needle help <command>" for more information about a command.
EOF
}
