#!/usr/bin/env bash
# NEEDLE CLI Config Subcommand
# View and modify configuration

_needle_config_help() {
    _needle_print "Usage: needle config <COMMAND> [OPTIONS]

View and modify NEEDLE configuration.

Commands:
    get <KEY>        Get a configuration value
    set <KEY> <VAL>  Set a configuration value
    list             List all configuration values
    edit             Open configuration in editor
    path             Show configuration file path

Options:
    -h, --help       Show this help message

Examples:
    needle config list              Show all config values
    needle config get editor        Get editor setting
    needle config set editor vim    Set editor to vim
    needle config edit              Open config in \$EDITOR
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
