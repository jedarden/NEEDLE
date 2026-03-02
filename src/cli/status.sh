#!/usr/bin/env bash
# NEEDLE CLI Status Subcommand
# Show current status and health of NEEDLE

_needle_status_help() {
    _needle_print "Show current status and health of NEEDLE

Displays information about the NEEDLE installation including
version, configuration status, and directory structure.

USAGE:
    needle status [OPTIONS]

OPTIONS:
    -j, --json       Output in JSON format
    -h, --help       Show this help message

EXAMPLES:
    # Show status
    needle status

    # Output as JSON for scripting
    needle status --json
"
}

_needle_status() {
    local json_output=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -j|--json)
                json_output=true
                shift
                ;;
            -h|--help)
                _needle_status_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                _needle_error "Unknown option: $1"
                _needle_status_help
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    # Gather status information
    local initialized="false"
    local config_exists="false"
    local state_dir_exists="false"
    local cache_dir_exists="false"

    if _needle_is_initialized; then
        initialized="true"
        [[ -f "$NEEDLE_HOME/$NEEDLE_CONFIG_FILE" ]] && config_exists="true"
        [[ -d "$NEEDLE_HOME/$NEEDLE_STATE_DIR" ]] && state_dir_exists="true"
        [[ -d "$NEEDLE_HOME/$NEEDLE_CACHE_DIR" ]] && cache_dir_exists="true"
    fi

    if [[ "$json_output" == "true" ]]; then
        cat << EOF
{
    "version": "$NEEDLE_VERSION",
    "home": "$NEEDLE_HOME",
    "initialized": $initialized,
    "paths": {
        "config": "$NEEDLE_HOME/$NEEDLE_CONFIG_FILE",
        "state": "$NEEDLE_HOME/$NEEDLE_STATE_DIR",
        "cache": "$NEEDLE_HOME/$NEEDLE_CACHE_DIR"
    },
    "exists": {
        "config": $config_exists,
        "state_dir": $state_dir_exists,
        "cache_dir": $cache_dir_exists
    }
}
EOF
        exit $NEEDLE_EXIT_SUCCESS
    fi

    _needle_header "NEEDLE Status"

    _needle_section "General"
    _needle_table_row "Version" "$NEEDLE_VERSION"
    _needle_table_row "Home" "$NEEDLE_HOME"
    _needle_table_row "Initialized" "$initialized"

    if [[ "$initialized" == "true" ]]; then
        _needle_section "Paths"
        _needle_table_row "Config" "$NEEDLE_HOME/$NEEDLE_CONFIG_FILE ($( [[ "$config_exists" == "true" ]] && echo "exists" || echo "missing" ))"
        _needle_table_row "State" "$NEEDLE_HOME/$NEEDLE_STATE_DIR ($( [[ "$state_dir_exists" == "true" ]] && echo "exists" || echo "missing" ))"
        _needle_table_row "Cache" "$NEEDLE_HOME/$NEEDLE_CACHE_DIR ($( [[ "$cache_dir_exists" == "true" ]] && echo "exists" || echo "missing" ))"
    fi

    exit $NEEDLE_EXIT_SUCCESS
}
