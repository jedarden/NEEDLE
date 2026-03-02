#!/usr/bin/env bash
# NEEDLE CLI Init Subcommand
# Initialize NEEDLE configuration and directory structure

_needle_init_help() {
    _needle_print "Usage: needle init [OPTIONS]

Initialize NEEDLE configuration and directory structure.

Options:
    -f, --force      Overwrite existing configuration
    -e, --editor     Set default editor
    -t, --timezone   Set timezone (default: UTC)
    -h, --help       Show this help message

Examples:
    needle init                    Initialize with defaults
    needle init --editor vim       Set editor to vim
    needle init --force            Reinitialize, overwriting config
"
}

_needle_init() {
    local force=false
    local editor="${EDITOR:-vim}"
    local timezone="UTC"

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -f|--force)
                force=true
                shift
                ;;
            -e|--editor)
                editor="$2"
                shift 2
                ;;
            -t|--timezone)
                timezone="$2"
                shift 2
                ;;
            -h|--help)
                _needle_init_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                _needle_error "Unknown option: $1"
                _needle_init_help
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    _needle_header "Initializing NEEDLE"

    # Check if already initialized
    if _needle_is_initialized && [[ "$force" != "true" ]]; then
        _needle_warn "NEEDLE is already initialized at $NEEDLE_HOME"
        _needle_info "Use --force to reinitialize"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # Create NEEDLE home directory
    if [[ ! -d "$NEEDLE_HOME" ]]; then
        _needle_verbose "Creating NEEDLE home: $NEEDLE_HOME"
        mkdir -p "$NEEDLE_HOME"
    fi

    # Create subdirectories
    _needle_ensure_dirs
    _needle_success "Created directory structure"

    # Create configuration file
    local config_file="$NEEDLE_HOME/$NEEDLE_CONFIG_FILE"
    if [[ -f "$config_file" ]] && [[ "$force" == "true" ]]; then
        _needle_verbose "Removing existing configuration"
        rm -f "$config_file"
    fi

    _needle_config_create_default "$config_file"

    # Update with provided values
    _needle_config_set "editor" "\"$editor\""
    _needle_config_set "timezone" "\"$timezone\""

    _needle_success "Created configuration file"

    # Create a simple README
    local readme="$NEEDLE_HOME/README.md"
    cat > "$readme" << EOF
# NEEDLE Configuration

This directory contains your NEEDLE configuration and state.

## Structure

- \`config.yaml\` - Main configuration file
- \`state/\` - Runtime state files
- \`cache/\` - Cached data
- \`logs/\` - Log files

## Configuration

Edit \`config.yaml\` to customize NEEDLE behavior.

## More Information

Run \`needle help\` for available commands.
EOF

    _needle_success "Created README"

    _needle_print ""
    _needle_success "NEEDLE initialized successfully!"
    _needle_info "Configuration: $NEEDLE_HOME/$NEEDLE_CONFIG_FILE"
    _needle_info "Run 'needle help' to see available commands"
}
