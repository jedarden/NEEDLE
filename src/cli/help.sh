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
        status)
            _needle_status_help
            ;;
        config)
            _needle_config_help
            ;;
        version)
            _needle_version_help
            ;;
        upgrade)
            _needle_upgrade_help
            ;;
        help)
            _needle_print "Usage: needle help [COMMAND]"
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
NEEDLE - Workflow Automation Tool

Usage:
    needle <command> [options] [arguments]

Commands:
    init        Initialize NEEDLE configuration
    run         Execute a workflow or script
    list        List available workflows and scripts
    status      Show NEEDLE status and health
    config      View and modify configuration
    version     Display version information
    upgrade     Upgrade NEEDLE to latest version
    help        Show help information

Global Options:
    -h, --help      Show this help message
    -v, --verbose   Enable verbose output
    -q, --quiet     Suppress non-essential output
    --no-color      Disable colored output
    --version       Show version number

Getting Started:
    needle init              Initialize NEEDLE
    needle run <workflow>    Execute a workflow
    needle list              List available workflows

Use "needle help <command>" for more information about a command.

Examples:
    needle init --editor vim
    needle run my-workflow --parallel --workers 8
    needle list workflows --format json
    needle config get editor

For more information, visit: https://github.com/example/needle
EOF
}
