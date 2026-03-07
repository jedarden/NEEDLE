#!/usr/bin/env bash
# NEEDLE CLI Agents Subcommand
# List and manage agent adapters from YAML configurations

_needle_agents_help() {
    _needle_print "List and manage agent adapters

Shows configured agent adapters and their availability status.
Can scan for new agents or test specific adapters.

USAGE:
    needle agents [OPTIONS]

OPTIONS:
    -s, --scan               Re-scan PATH for available agents
    -j, --json               Output as JSON
    -a, --all                Include unavailable agents

    -h, --help               Print help information

OUTPUT:
    NAME         Agent adapter name (e.g., claude-anthropic-sonnet)
    RUNNER       CLI executable (e.g., claude)
    STATUS       available | missing | auth-required
    VERSION      Detected version if available

EXAMPLES:
    # List configured agents
    needle agents

    # Scan for new agents
    needle agents --scan

    # JSON output
    needle agents --json

SEE ALSO:
    needle test-agent    Test a specific agent adapter
"
}

# Output agents as JSON array
_needle_agents_json() {
    local -a agents=("$@")
    local json="["
    local first=true

    for agent_info in "${agents[@]}"; do
        IFS='|' read -r name runner status source file <<< "$agent_info"

        if [[ "$first" == "true" ]]; then
            first=false
        else
            json+=","
        fi

        json+="{"
        json+="\"name\":\"$(_needle_json_escape "$name")\","
        json+="\"runner\":\"$(_needle_json_escape "$runner")\","
        json+="\"status\":\"$(_needle_json_escape "$status")\","
        json+="\"source\":\"$(_needle_json_escape "$source")\""
        json+="}"
    done

    json+="]"
    echo "$json"
}

_needle_agents() {
    local scan=false
    local json_output=false
    local show_all=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -s|--scan)
                scan=true
                shift
                ;;
            -j|--json)
                json_output=true
                shift
                ;;
            -a|--all)
                show_all=true
                shift
                ;;
            -h|--help)
                _needle_agents_help
                return 0
                ;;
            -*)
                _needle_error "Unknown option: $1"
                _needle_agents_help
                return 1
                ;;
            *)
                _needle_error "Unexpected argument: $1"
                _needle_agents_help
                return 1
                ;;
        esac
    done

    # Initialize agent loader if needed
    [[ -z "$NEEDLE_BUILTIN_AGENTS_DIR" ]] && _needle_agent_loader_init

    if $scan; then
        _needle_info "Scanning for agents..."
    fi

    # Collect agents from builtin and user directories
    local -a agents=()
    local -A seen=()

    # Built-in agents directory
    if [[ -n "$NEEDLE_BUILTIN_AGENTS_DIR" && -d "$NEEDLE_BUILTIN_AGENTS_DIR" ]]; then
        for file in "$NEEDLE_BUILTIN_AGENTS_DIR"/*.yaml "$NEEDLE_BUILTIN_AGENTS_DIR"/*.yml; do
            [[ -f "$file" ]] || continue
            local basename
            basename=$(basename "$file")
            basename="${basename%.yaml}"
            basename="${basename%.yml}"

            # Skip if already seen (user config takes precedence)
            if [[ -z "${seen[$basename]:-}" ]]; then
                seen[$basename]=1
                agents+=("$basename|builtin|$file")
            fi
        done 2>/dev/null
    fi

    # User agents directory
    local user_agents_dir="${NEEDLE_HOME}/agents"
    if [[ -d "$user_agents_dir" ]]; then
        for file in "$user_agents_dir"/*.yaml "$user_agents_dir"/*.yml; do
            [[ -f "$file" ]] || continue
            local basename
            basename=$(basename "$file")
            basename="${basename%.yaml}"
            basename="${basename%.yml}"

            # User config takes precedence - replace if exists
            if [[ -n "${seen[$basename]:-}" ]]; then
                # Remove existing entry and add user version
                local -a new_agents=()
                for agent_info in "${agents[@]}"; do
                    IFS='|' read -r name source _ <<< "$agent_info"
                    if [[ "$name" != "$basename" ]]; then
                        new_agents+=("$agent_info")
                    fi
                done
                agents=("${new_agents[@]}")
            fi
            agents+=("$basename|user|$file")
            seen[$basename]=1
        done 2>/dev/null
    fi

    # Sort agents by name
    IFS=$'\n' agents=($(sort -t'|' -k1 <<<"${agents[*]}")); unset IFS

    if $json_output; then
        # Build JSON with status info
        local -a json_agents=()
        for agent_info in "${agents[@]}"; do
            IFS='|' read -r name source file <<< "$agent_info"

            local runner
            runner=$(_needle_parse_yaml "$file" '.runner' 2>/dev/null)
            [[ -z "$runner" ]] && runner="unknown"

            local status
            if command -v "$runner" &>/dev/null; then
                status="available"
            else
                status="missing"
                ! $show_all && continue
            fi

            json_agents+=("$name|$runner|$status|$source|$file")
        done

        _needle_agents_json "${json_agents[@]}"
    else
        # Human-readable table output
        printf "%-35s %-10s %-12s %s\n" "NAME" "RUNNER" "STATUS" "SOURCE"
        printf "%-35s %-10s %-12s %s\n" "----" "------" "------" "------"

        local found=0
        for agent_info in "${agents[@]}"; do
            IFS='|' read -r name source file <<< "$agent_info"

            local runner
            runner=$(_needle_parse_yaml "$file" '.runner' 2>/dev/null)
            [[ -z "$runner" ]] && runner="unknown"

            local status
            if command -v "$runner" &>/dev/null; then
                status="available"
                ((found++)) || true
            else
                status="missing"
                ! $show_all && continue
            fi

            # Colorize status
            local status_display
            if [[ "$status" == "available" ]]; then
                status_display="${NEEDLE_COLOR_GREEN}available${NEEDLE_COLOR_RESET}"
            else
                status_display="${NEEDLE_COLOR_RED}missing${NEEDLE_COLOR_RESET}"
            fi

            printf "%-35s %-10s %-12s %s\n" "$name" "$runner" "$status_display" "$source"
        done

        # Summary
        if [[ $found -eq 0 ]]; then
            _needle_print ""
            if $show_all; then
                _needle_warn "No agents available"
            else
                _needle_warn "No available agents found"
                _needle_info "Use --all to see all configured agents"
            fi
        fi
    fi

    return 0
}
