#!/usr/bin/env bash
# NEEDLE CLI Agents Subcommand
# Manage and detect coding CLI agents

_needle_agents_help() {
    _needle_print "Usage: needle agents [OPTIONS] [COMMAND]

Manage and detect coding CLI agents.

Commands:
    scan         Scan for installed agents (default)
    list         List all installed agents
    info         Show detailed info for an agent
    default      Show the default agent

Options:
    -j, --json   Output in JSON format
    -h, --help   Show this help message

Examples:
    needle agents              Scan for all agents
    needle agents scan         Scan for all agents
    needle agents list         List installed agents
    needle agents info claude  Show info for Claude
    needle agents --json       Output scan as JSON
"
}

_needle_agents() {
    local json_output=false
    local command="scan"

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -j|--json)
                json_output=true
                shift
                ;;
            -h|--help)
                _needle_agents_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            scan|list|info|default)
                command="$1"
                shift
                ;;
            -*)
                _needle_error "Unknown option: $1"
                _needle_agents_help
                exit $NEEDLE_EXIT_USAGE
                ;;
            *)
                # Positional argument (for info command)
                shift
                ;;
        esac
    done

    case "$command" in
        scan)
            if [[ "$json_output" == "true" ]]; then
                _needle_scan_agents --json
            else
                _needle_scan_agents
            fi
            ;;
        list)
            if [[ "$json_output" == "true" ]]; then
                _needle_scan_agents_json
            else
                _needle_list_agents
            fi
            ;;
        info)
            local agent="${1:-}"
            if [[ -z "$agent" ]]; then
                # Show info for default agent
                agent=$(_needle_get_default_agent 2>/dev/null)
                if [[ -z "$agent" ]]; then
                    _needle_error "No agent specified and no default agent found"
                    _needle_info "Usage: needle agents info <agent_name>"
                    exit $NEEDLE_EXIT_USAGE
                fi
            fi
            _needle_agent_info "$agent"
            ;;
        default)
            local default_agent
            default_agent=$(_needle_get_default_agent 2>/dev/null)
            if [[ -z "$default_agent" ]]; then
                _needle_warn "No ready agent found"
                _needle_info "Install and configure an agent first"
                exit $NEEDLE_EXIT_ERROR
            fi

            if [[ "$json_output" == "true" ]]; then
                echo "{\"default_agent\":\"$default_agent\"}"
            else
                _needle_print "$default_agent"
            fi
            ;;
    esac
}

# List installed agents (simple format)
_needle_list_agents() {
    local installed
    installed=$(_needle_get_installed_agents)

    if [[ -z "$installed" ]]; then
        _needle_warn "No agents installed"
        _needle_info "Run 'needle agents scan' for installation instructions"
        return 0
    fi

    _needle_section "Installed Agents"

    for agent in $installed; do
        local name="${NEEDLE_AGENT_NAMES[$agent]:-$agent}"
        local version
        version=$(_needle_agent_version "$agent" 2>/dev/null)
        local auth
        auth=$(_needle_agent_auth_status "$agent" 2>/dev/null)

        local auth_indicator
        case "$auth" in
            authenticated)
                auth_indicator="${NEEDLE_COLOR_GREEN}✓${NEEDLE_COLOR_RESET}"
                ;;
            auth-required)
                auth_indicator="${NEEDLE_COLOR_YELLOW}!${NEEDLE_COLOR_RESET}"
                ;;
            *)
                auth_indicator="${NEEDLE_COLOR_DIM}?${NEEDLE_COLOR_RESET}"
                ;;
        esac

        printf "  %s %-12s %s %s\n" "$auth_indicator" "$name" "${NEEDLE_COLOR_DIM}$version${NEEDLE_COLOR_RESET}" ""
    done

    _needle_print ""
    _needle_info "Legend: ✓ authenticated  ! auth required  ? unknown"
}
