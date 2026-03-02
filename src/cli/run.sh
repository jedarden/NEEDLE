#!/usr/bin/env bash
# NEEDLE CLI Run Subcommand
# Execute a needle workflow or script

_needle_run_help() {
    _needle_print "Start a worker to process beads from the queue

Starts a NEEDLE worker that processes beads (tasks) from the queue.
The worker will claim beads, execute them with the configured agent,
and mark them as complete.

USAGE:
    needle run [OPTIONS]

OPTIONS:
    -w, --workspace <PATH>   Workspace directory containing .beads/
    -a, --agent <NAME>       Agent to use (e.g., claude-anthropic-sonnet)
    -p, --parallel           Run in parallel mode
    -n, --workers <NUM>      Number of parallel workers (default: 4)
    -d, --dry-run            Show what would be done without executing
    -f, --force              Skip concurrency limit checks
    --status                 Show concurrency status for agent
    -v, --verbose            Show detailed output
    -h, --help               Show this help message

EXAMPLES:
    # Start a worker with explicit options
    needle run --workspace=/path/to/project --agent=claude-anthropic-sonnet

    # Run with parallel execution
    needle run -w /path/to/project -a claude-anthropic-sonnet -p

    # Skip concurrency limits (use with caution)
    needle run --workspace=/path/to/project --agent=claude-anthropic-sonnet --force

    # Preview what would be done
    needle run --dry-run

    # Check concurrency status before starting
    needle run --agent=claude-anthropic-sonnet --status

CONCURRENCY LIMITS:
    NEEDLE enforces three levels of concurrency limits:

    1. Global limit     - Maximum total workers across all providers
    2. Provider limit   - Maximum workers per provider (e.g., anthropic)
    3. Model limit      - Maximum workers per agent (e.g., claude-anthropic-sonnet)

    Limits are configured in ~/.needle/config.yaml:

    limits:
      global_max_concurrent: 20
      providers:
        anthropic:
          max_concurrent: 5
      models:
        claude-anthropic-opus:
          max_concurrent: 2

    Use --force to bypass these checks (not recommended for normal use).
"
}

_needle_run() {
    local workspace=""
    local agent=""
    local parallel=false
    local workers=4
    local dry_run=false
    local force=false
    local show_status=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -w|--workspace)
                workspace="$2"
                shift 2
                ;;
            -a|--agent)
                agent="$2"
                shift 2
                ;;
            -p|--parallel)
                parallel=true
                shift
                ;;
            -n|--workers)
                workers="$2"
                shift 2
                ;;
            -d|--dry-run)
                dry_run=true
                shift
                ;;
            -f|--force)
                force=true
                shift
                ;;
            --status)
                show_status=true
                shift
                ;;
            -v|--verbose)
                NEEDLE_VERBOSE=true
                shift
                ;;
            -h|--help)
                _needle_run_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            -*)
                _needle_error "Unknown option: $1"
                _needle_run_help
                exit $NEEDLE_EXIT_USAGE
                ;;
            *)
                if [[ -z "$workspace" ]]; then
                    workspace="$1"
                fi
                shift
                ;;
        esac
    done

    # Handle --status flag
    if [[ "$show_status" == "true" ]]; then
        if [[ -z "$agent" ]]; then
            _needle_error "Agent required for status check. Use: needle run --agent=<name> --status"
            exit $NEEDLE_EXIT_USAGE
        fi

        # Load agent to get provider
        local provider=""
        if _needle_load_agent "$agent" &>/dev/null; then
            provider="${NEEDLE_AGENT[provider]:-}"
        fi

        _needle_show_concurrency_status "$agent" "$provider"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # Validate workspace
    if [[ -n "$workspace" ]] && [[ ! -d "$workspace" ]]; then
        _needle_error "Workspace not found: $workspace"
        exit $NEEDLE_EXIT_USAGE
    fi

    # Validate agent
    if [[ -n "$agent" ]]; then
        if ! _needle_find_agent_config "$agent" &>/dev/null; then
            _needle_error "Agent not found: $agent"
            _needle_info "Run 'needle agents list' to see available agents"
            exit $NEEDLE_EXIT_USAGE
        fi

        # Load agent configuration
        if ! _needle_load_agent "$agent"; then
            _needle_error "Failed to load agent: $agent"
            exit $NEEDLE_EXIT_CONFIG
        fi

        local provider="${NEEDLE_AGENT[provider]:-}"

        # Check concurrency limits (unless --force or --dry-run)
        if [[ "$force" != "true" ]] && [[ "$dry_run" != "true" ]]; then
            _needle_verbose "Checking concurrency limits..."

            if ! _needle_check_concurrency "$agent" "$provider" "$workers"; then
                _needle_error "$NEEDLE_LIMIT_CHECK_MESSAGE"
                _needle_info "$NEEDLE_LIMIT_CHECK_DETAILS"
                _needle_print ""
                _needle_info "Use --force to bypass this check (not recommended)"
                _needle_info "Use --status to see current limit usage"
                exit $NEEDLE_EXIT_USAGE
            fi

            _needle_success "Concurrency check passed"
            _needle_verbose "$NEEDLE_LIMIT_CHECK_DETAILS"
        fi

        _needle_header "Running: $agent"
    else
        _needle_header "Running: default"
    fi

    if [[ "$dry_run" == "true" ]]; then
        _needle_info "Dry run mode - no changes will be made"
    fi

    _needle_verbose "Workspace: ${workspace:-$(pwd)}"
    _needle_verbose "Agent: ${agent:-default}"
    _needle_verbose "Parallel: $parallel"
    _needle_verbose "Workers: $workers"
    _needle_verbose "Force: $force"

    # TODO: Implement actual workflow execution
    _needle_warn "Workflow execution not yet implemented"
    _needle_info "This is a stub for the 'run' subcommand"

    # Show what would be done in dry-run mode
    if [[ "$dry_run" == "true" ]]; then
        _needle_print ""
        _needle_section "Dry Run Summary"
        [[ -n "$agent" ]] && _needle_table_row "Agent" "$agent"
        [[ -n "$provider" ]] && _needle_table_row "Provider" "$provider"
        _needle_table_row "Workers to start" "$workers"
        _needle_table_row "Parallel mode" "$parallel"

        if [[ -n "$agent" ]] && [[ -n "$provider" ]]; then
            _needle_print ""
            local status_json
            status_json=$(_needle_get_concurrency_status "$agent" "$provider")
            local global_current global_limit
            global_current=$(echo "$status_json" | jq -r '.global.current')
            global_limit=$(echo "$status_json" | jq -r '.global.limit')
            _needle_table_row "Current global workers" "$global_current / $global_limit"
        fi
    fi

    exit $NEEDLE_EXIT_SUCCESS
}
