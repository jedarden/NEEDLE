#!/usr/bin/env bash
# NEEDLE CLI List Subcommand
# List available workflows, scripts, or resources

_needle_list_help() {
    _needle_print "List running workers and available resources

Shows information about active NEEDLE workers and available
workflows, scripts, or other resources.

USAGE:
    needle list [TYPE] [OPTIONS]

ARGUMENTS:
    TYPE             What to list: workers, workflows, scripts, all
                     (default: workers)

OPTIONS:
    -f, --format <FMT>   Output format: table, json, simple (default: table)
    -q, --quiet          Only show names (one per line)
    -h, --help           Show this help message

EXAMPLES:
    # List running workers
    needle list

    # List all resources
    needle list all

    # Output as JSON for scripting
    needle list --format json

    # Get just the names
    needle list --quiet
"
}

_needle_list() {
    local type="all"
    local format="table"
    local quiet=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -f|--format)
                format="$2"
                shift 2
                ;;
            -q|--quiet)
                quiet=true
                shift
                ;;
            -h|--help)
                _needle_list_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            workflows|scripts|all)
                type="$1"
                shift
                ;;
            -*)
                _needle_error "Unknown option: $1"
                _needle_list_help
                exit $NEEDLE_EXIT_USAGE
                ;;
            *)
                _needle_error "Unknown type: $1"
                _needle_list_help
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    if [[ "$quiet" == "true" ]]; then
        # Output just names for scripting
        echo "example-workflow"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    _needle_header "Available Resources"

    if [[ "$type" == "all" || "$type" == "workflows" ]]; then
        _needle_section "Workflows"
        if [[ "$format" == "json" ]]; then
            echo '{"workflows": [{"name": "example-workflow", "description": "Example workflow"}]}'
        else
            _needle_table_row "example-workflow" "Example workflow"
        fi
    fi

    if [[ "$type" == "all" || "$type" == "scripts" ]]; then
        _needle_section "Scripts"
        if [[ "$format" == "json" ]]; then
            echo '{"scripts": []}'
        else
            _needle_info "No scripts available"
        fi
    fi

    # TODO: Implement actual listing from filesystem
    _needle_verbose "Type: $type, Format: $format"

    exit $NEEDLE_EXIT_SUCCESS
}
