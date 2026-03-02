#!/usr/bin/env bash
# NEEDLE CLI Version Subcommand
# Display version information

_needle_version_help() {
    _needle_print "Usage: needle version [OPTIONS]

Display NEEDLE version information.

Options:
    -j, --json       Output in JSON format
    -s, --short      Output short version string only
    -h, --help       Show this help message

Examples:
    needle version          Show full version info
    needle version --short  Show just version number
    needle version --json   Output as JSON
"
}

_needle_version() {
    local json_output=false
    local short=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -j|--json)
                json_output=true
                shift
                ;;
            -s|--short)
                short=true
                shift
                ;;
            -h|--help)
                _needle_version_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                _needle_error "Unknown option: $1"
                _needle_version_help
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    if [[ "$short" == "true" ]]; then
        echo "$NEEDLE_VERSION"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    if [[ "$json_output" == "true" ]]; then
        cat << EOF
{
    "version": "$NEEDLE_VERSION",
    "major": $NEEDLE_VERSION_MAJOR,
    "minor": $NEEDLE_VERSION_MINOR,
    "patch": $NEEDLE_VERSION_PATCH
}
EOF
        exit $NEEDLE_EXIT_SUCCESS
    fi

    _needle_print "NEEDLE v$NEEDLE_VERSION"
    _needle_print "  Major: $NEEDLE_VERSION_MAJOR"
    _needle_print "  Minor: $NEEDLE_VERSION_MINOR"
    _needle_print "  Patch: $NEEDLE_VERSION_PATCH"

    exit $NEEDLE_EXIT_SUCCESS
}
