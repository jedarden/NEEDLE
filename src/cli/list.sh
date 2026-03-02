#!/usr/bin/env bash
# NEEDLE CLI List Subcommand
# List available workflows, scripts, or resources

_needle_list_help() {
    _needle_print "Usage: needle list [TYPE] [OPTIONS]

List available workflows, scripts, or resources.

Arguments:
    TYPE             What to list: workflows, scripts, all (default: all)

Options:
    -f, --format     Output format: table, json, simple (default: table)
    -q, --quiet      Only show names
    -h, --help       Show this help message

Examples:
    needle list                  List all resources
    needle list workflows        List only workflows
    needle list --format json    Output as JSON
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
