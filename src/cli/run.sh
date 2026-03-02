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
    -v, --verbose            Show detailed output
    -h, --help               Show this help message

EXAMPLES:
    # Start a worker with explicit options
    needle run --workspace=/path/to/project --agent=claude-anthropic-sonnet

    # Run with parallel execution
    needle run -w /path/to/project -a claude-anthropic-sonnet -p

    # Preview what would be done
    needle run --dry-run
"
}

_needle_run() {
    local workflow=""
    local parallel=false
    local workers=4
    local dry_run=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -p|--parallel)
                parallel=true
                shift
                ;;
            -w|--workers)
                workers="$2"
                shift 2
                ;;
            -d|--dry-run)
                dry_run=true
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
                if [[ -z "$workflow" ]]; then
                    workflow="$1"
                fi
                shift
                ;;
        esac
    done

    # Validate workflow name
    if [[ -z "$workflow" ]]; then
        _needle_error "No workflow specified"
        _needle_run_help
        exit $NEEDLE_EXIT_USAGE
    fi

    _needle_header "Running: $workflow"

    if [[ "$dry_run" == "true" ]]; then
        _needle_info "Dry run mode - no changes will be made"
    fi

    _needle_verbose "Parallel: $parallel"
    _needle_verbose "Workers: $workers"

    # TODO: Implement actual workflow execution
    _needle_warn "Workflow execution not yet implemented"
    _needle_info "This is a stub for the 'run' subcommand"

    exit $NEEDLE_EXIT_SUCCESS
}
