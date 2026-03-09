#!/usr/bin/env bash
# NEEDLE CLI Config Subcommand
# View and modify configuration

_needle_config_help() {
    _needle_print "View or edit NEEDLE configuration

Display current configuration or open editor for modifications.

USAGE:
    needle config [COMMAND] [OPTIONS]

COMMANDS:
    show        Display current configuration (default)
    edit        Open configuration in editor
    validate    Validate configuration syntax
    path        Show configuration file paths

OPTIONS:
    --global            Target global config (~/.needle/config.yaml)
    --workspace         Target workspace config (.needle.yaml)

    -j, --json          Output as JSON (for 'show')

    -h, --help          Print help information

EXAMPLES:
    # Show current config
    needle config show

    # Edit global config
    needle config edit --global

    # Validate configuration
    needle config validate

    # Show config file paths
    needle config path
"
}

# Help for config show subcommand
_needle_config_show_help() {
    _needle_print "Display current configuration

Show the contents of the configuration file. By default shows
the global configuration, but can target workspace config.

USAGE:
    needle config show [OPTIONS]

OPTIONS:
    --global       Show global config (~/.needle/config.yaml)
    --workspace    Show workspace config (.needle.yaml)
    -j, --json     Output as JSON format
    -h, --help     Show this help message

EXAMPLES:
    # Show global configuration
    needle config show

    # Show workspace configuration
    needle config show --workspace

    # Get JSON output for scripting
    needle config show --json
"
}

# Display current configuration
_needle_config_show() {
    local json_output=false
    local use_global=true
    local use_workspace=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            -j|--json) json_output=true; shift ;;
            --global) use_global=true; use_workspace=false; shift ;;
            --workspace) use_workspace=true; use_global=false; shift ;;
            -h|--help) _needle_config_show_help; exit $NEEDLE_EXIT_SUCCESS ;;
            *) shift ;;
        esac
    done

    local config_file
    if $use_workspace; then
        config_file=".needle.yaml"
        if [[ ! -f "$config_file" ]]; then
            _needle_error "Workspace config not found: $config_file"
            exit $NEEDLE_EXIT_CONFIG
        fi
    else
        config_file="$NEEDLE_CONFIG_FILE"
        if [[ ! -f "$config_file" ]]; then
            _needle_error "Configuration not initialized. Run 'needle init' first."
            exit $NEEDLE_EXIT_CONFIG
        fi
    fi

    if $json_output; then
        if command -v yq &>/dev/null; then
            yq -o=json '.' "$config_file" 2>/dev/null
        elif command -v jq &>/dev/null; then
            # Try python YAML to JSON conversion as fallback
            if command -v python3 &>/dev/null; then
                python3 -c "import yaml, json, sys; print(json.dumps(yaml.safe_load(open('$config_file')), indent=2))" 2>/dev/null
            else
                _needle_error "JSON output requires yq or python3 with PyYAML"
                exit $NEEDLE_EXIT_ERROR
            fi
        else
            _needle_error "JSON output requires yq or jq"
            exit $NEEDLE_EXIT_ERROR
        fi
    else
        cat "$config_file"
    fi
}

# Help for config edit subcommand
_needle_config_edit_help() {
    _needle_print "Open configuration in editor

Open the configuration file in your default editor (\$EDITOR or vim).
By default edits the global configuration.

USAGE:
    needle config edit [OPTIONS]

OPTIONS:
    --global       Edit global config (~/.needle/config.yaml)
    --workspace    Edit workspace config (.needle.yaml)
    -h, --help     Show this help message

EXAMPLES:
    # Edit global configuration
    needle config edit

    # Edit workspace configuration
    needle config edit --workspace

    # Use a specific editor
    EDITOR=nano needle config edit
"
}

# Edit configuration file
_needle_config_edit() {
    local use_global=true

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --global) use_global=true; shift ;;
            --workspace) use_global=false; shift ;;
            -h|--help) _needle_config_edit_help; exit $NEEDLE_EXIT_SUCCESS ;;
            *) shift ;;
        esac
    done

    local config_file
    if $use_global; then
        config_file="$NEEDLE_CONFIG_FILE"
        if [[ ! -f "$config_file" ]]; then
            _needle_error "Configuration not initialized. Run 'needle init' first."
            exit $NEEDLE_EXIT_CONFIG
        fi
    else
        config_file=".needle.yaml"
        if [[ ! -f "$config_file" ]]; then
            _needle_warn "Workspace config not found, creating: $config_file"
            touch "$config_file"
        fi
    fi

    local editor="${EDITOR:-vim}"
    editor=$(_needle_config_get "editor" 2>/dev/null || echo "$editor")
    ${editor} "$config_file"
}

# Help for config validate subcommand
_needle_config_validate_help() {
    _needle_print "Validate configuration syntax

Check the configuration file for valid YAML syntax and report
any errors found.

USAGE:
    needle config validate [OPTIONS]

OPTIONS:
    -h, --help     Show this help message

EXAMPLES:
    # Validate default configuration
    needle config validate

    # Validate after editing
    needle config edit && needle config validate
"
}

# Validate configuration syntax
_needle_config_validate() {
    # Check for help flag first
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -h|--help)
                _needle_config_validate_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                shift
                ;;
        esac
    done

    local config_file="${NEEDLE_CONFIG_FILE}"

    if [[ ! -f "$config_file" ]]; then
        _needle_error "Configuration file not found: $config_file"
        exit $NEEDLE_EXIT_CONFIG
    fi

    if [[ ! -s "$config_file" ]]; then
        _needle_error "Configuration file is empty: $config_file"
        exit $NEEDLE_EXIT_CONFIG
    fi

    # Validate YAML syntax using yq
    if command -v yq &>/dev/null; then
        if yq eval '.' "$config_file" &>/dev/null; then
            _needle_success "Configuration is valid"
            return 0
        else
            _needle_error "Configuration has YAML syntax errors"
            yq eval '.' "$config_file" 2>&1 | head -5
            exit $NEEDLE_EXIT_CONFIG
        fi
    # Fallback to python YAML validation
    elif command -v python3 &>/dev/null; then
        if python3 -c "import yaml; yaml.safe_load(open('$config_file'))" 2>/dev/null; then
            _needle_success "Configuration is valid"
            return 0
        else
            _needle_error "Configuration has YAML syntax errors"
            python3 -c "import yaml; yaml.safe_load(open('$config_file'))" 2>&1
            exit $NEEDLE_EXIT_CONFIG
        fi
    else
        _needle_warn "Cannot validate YAML syntax (yq or python3 required)"
        _needle_info "File exists and is non-empty: $config_file"
        return 0
    fi
}

# Help for config path subcommand
_needle_config_path_help() {
    _needle_print "Show configuration file paths

Display the paths to all configuration files and directories
used by NEEDLE.

USAGE:
    needle config path [OPTIONS]

OPTIONS:
    -h, --help     Show this help message

EXAMPLES:
    # Show all configuration paths
    needle config path

    # Use in scripts
    needle config path | grep 'Global'
"
}

# Show all configuration paths
_needle_config_path() {
    # Check for help flag first
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -h|--help)
                _needle_config_path_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                shift
                ;;
        esac
    done

    _needle_section "Configuration Paths"
    _needle_table_row "Global config" "$NEEDLE_CONFIG_FILE"
    _needle_table_row "Workspace config" ".needle.yaml"
    _needle_table_row "Logs" "$NEEDLE_HOME/$NEEDLE_LOG_DIR"
    _needle_table_row "State" "$NEEDLE_HOME/$NEEDLE_STATE_DIR"
    _needle_table_row "Cache" "$NEEDLE_HOME/$NEEDLE_CACHE_DIR"
}

_needle_config() {
    local command="${1:-show}"
    shift || true

    case "$command" in
        show)
            _needle_config_show "$@"
            ;;

        list)
            # Alias for show (backward compatibility)
            _needle_config_show "$@"
            ;;

        get)
            local key="${1:-}"
            if [[ -z "$key" ]]; then
                _needle_error "No key specified"
                _needle_config_help
                exit $NEEDLE_EXIT_USAGE
            fi
            local value
            value=$(_needle_config_get "$key")
            if [[ -n "$value" ]]; then
                echo "$value"
            else
                _needle_error "Key not found: $key"
                exit $NEEDLE_EXIT_ERROR
            fi
            ;;

        set)
            local key="${1:-}"
            local value="${2:-}"
            if [[ -z "$key" ]]; then
                _needle_error "No key specified"
                _needle_config_help
                exit $NEEDLE_EXIT_USAGE
            fi
            if [[ -z "$value" ]]; then
                _needle_error "No value specified"
                _needle_config_help
                exit $NEEDLE_EXIT_USAGE
            fi
            _needle_config_set "$key" "$value"
            _needle_success "Set $key = $value"
            ;;

        edit)
            _needle_config_edit "$@"
            ;;

        validate)
            _needle_config_validate "$@"
            ;;

        path)
            _needle_config_path "$@"
            ;;

        -h|--help|help)
            _needle_config_help
            exit $NEEDLE_EXIT_SUCCESS
            ;;

        *)
            _needle_error "Unknown command: $command"
            _needle_config_help
            exit $NEEDLE_EXIT_USAGE
            ;;
    esac
}
