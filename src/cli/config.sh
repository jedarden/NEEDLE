#!/usr/bin/env bash
# NEEDLE CLI Config Subcommand
# View and modify configuration

_needle_config_help() {
    _needle_print "View and modify NEEDLE configuration

Allows viewing and editing the NEEDLE configuration file.
Configuration is stored in YAML format at ~/.needle/config.yaml.

USAGE:
    needle config <COMMAND> [OPTIONS]

COMMANDS:
    get <KEY>        Get a configuration value
    set <KEY> <VAL>  Set a configuration value
    list             List all configuration values
    edit             Open configuration in \$EDITOR
    path             Show configuration file path

OPTIONS:
    -h, --help       Show this help message

EXAMPLES:
    # Show all config values
    needle config list

    # Get a specific value
    needle config get editor

    # Set a value
    needle config set editor vim

    # Open config in editor
    needle config edit
"
}

_needle_config() {
    local command="${1:-list}"
    shift || true

    case "$command" in
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

        list)
            local config_file="$NEEDLE_HOME/$NEEDLE_CONFIG_FILE"
            if [[ ! -f "$config_file" ]]; then
                _needle_error "Configuration not initialized. Run 'needle init' first."
                exit $NEEDLE_EXIT_CONFIG
            fi
            cat "$config_file"
            ;;

        edit)
            local config_file="$NEEDLE_HOME/$NEEDLE_CONFIG_FILE"
            if [[ ! -f "$config_file" ]]; then
                _needle_error "Configuration not initialized. Run 'needle init' first."
                exit $NEEDLE_EXIT_CONFIG
            fi
            local editor="${EDITOR:-vim}"
            editor=$(_needle_config_get "editor" 2>/dev/null || echo "$editor")
            ${editor} "$config_file"
            ;;

        path)
            echo "$NEEDLE_HOME/$NEEDLE_CONFIG_FILE"
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
