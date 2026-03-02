#!/usr/bin/env bash
# NEEDLE CLI Configuration Management
# Load, validate, and manage configuration with defaults merging

# Config file location
NEEDLE_CONFIG_FILE="${NEEDLE_CONFIG_FILE:-$NEEDLE_HOME/$NEEDLE_CONFIG_NAME}"

# Cache variable (not exported, only for this session)
NEEDLE_CONFIG_CACHE=""

# Default configuration as JSON (used for merging)
_NEEDLE_CONFIG_DEFAULTS='{
  "limits": {
    "global_max_concurrent": 20,
    "providers": {
      "anthropic": {
        "max_concurrent": 5
      }
    }
  },
  "runner": {
    "polling_interval": "2s",
    "idle_timeout": "300s"
  },
  "strands": {
    "pluck": true,
    "explore": true,
    "mend": true,
    "weave": false,
    "unravel": false,
    "pulse": false,
    "knot": true
  },
  "effort": {
    "budget": {
      "daily_limit_usd": 50.0,
      "warning_threshold": 0.8
    }
  }
}'

# Default configuration as YAML (for creating new configs)
_NEEDLE_CONFIG_DEFAULTS_YAML='# NEEDLE Configuration
# See: https://github.com/user/needle#configuration

limits:
  global_max_concurrent: 20
  providers:
    anthropic:
      max_concurrent: 5

runner:
  polling_interval: 2s
  idle_timeout: 300s

strands:
  pluck: true
  explore: true
  mend: true
  weave: false
  unravel: false
  pulse: false
  knot: true

effort:
  budget:
    daily_limit_usd: 50.0
    warning_threshold: 0.8
'

# Check if yq is available
_needle_has_yq() {
    command -v yq &>/dev/null
}

# Check if NEEDLE is initialized
_needle_is_initialized() {
    [[ -f "$NEEDLE_CONFIG_FILE" ]]
}

# Check if config exists and is valid
# Usage: config_exists
config_exists() {
    [[ -f "$NEEDLE_CONFIG_FILE" ]] && [[ -s "$NEEDLE_CONFIG_FILE" ]]
}

# Load and cache config with defaults merged
# Uses NEEDLE_CONFIG_FILE env var (defaults to ~/.needle/config.yaml)
# Returns: JSON format configuration
# Usage: load_config
load_config() {
    # Return cached config if available
    if [[ -n "$NEEDLE_CONFIG_CACHE" ]]; then
        echo "$NEEDLE_CONFIG_CACHE"
        return 0
    fi

    if _needle_has_yq; then
        # Use yq for YAML processing (preferred)
        if [[ -f "$NEEDLE_CONFIG_FILE" ]]; then
            # Merge defaults with user config (user config overrides defaults)
            NEEDLE_CONFIG_CACHE=$(echo "$_NEEDLE_CONFIG_DEFAULTS" | yq eval-all 'select(fileIndex==0) * select(fileIndex==1)' - "$NEEDLE_CONFIG_FILE" 2>/dev/null)
            if [[ $? -ne 0 ]]; then
                # If merge fails, just use defaults
                _needle_warn "Config merge failed, using defaults"
                NEEDLE_CONFIG_CACHE="$_NEEDLE_CONFIG_DEFAULTS"
            fi
        else
            # No config file, use defaults
            NEEDLE_CONFIG_CACHE="$_NEEDLE_CONFIG_DEFAULTS"
        fi
    else
        # Fallback without yq: use defaults and try to parse simple YAML
        if [[ -f "$NEEDLE_CONFIG_FILE" ]]; then
            # Simple merge: start with defaults, override with found values
            NEEDLE_CONFIG_CACHE=$(_needle_simple_yaml_merge "$NEEDLE_CONFIG_FILE")
        else
            NEEDLE_CONFIG_CACHE="$_NEEDLE_CONFIG_DEFAULTS"
        fi
    fi

    echo "$NEEDLE_CONFIG_CACHE"
}

# Simple YAML merge fallback (without yq)
# Parses basic key:value pairs and updates defaults
_needle_simple_yaml_merge() {
    local config_file="$1"
    local result="$_NEEDLE_CONFIG_DEFAULTS"

    # Read simple top-level settings and update
    # This is a simplified implementation for basic configs
    while IFS= read -r line; do
        # Skip comments and empty lines
        [[ "$line" =~ ^[[:space:]]*# ]] && continue
        [[ -z "${line// }" ]] && continue

        # Handle simple key: value pairs (top level only)
        if [[ "$line" =~ ^([a-z_]+):[[:space:]]*(.*)$ ]]; then
            local key="${BASH_REMATCH[1]}"
            local value="${BASH_REMATCH[2]}"

            # Remove quotes from value
            value="${value#\"}"
            value="${value%\"}"

            # Update JSON using simple string replacement
            # Note: This is limited to top-level keys
            case "$key" in
                global_max_concurrent|timeout|max_workers)
                    if [[ "$value" =~ ^[0-9]+$ ]]; then
                        result=$(echo "$result" | sed "s/\"$key\":[^,}]*/\"$key\": $value/")
                    fi
                    ;;
                polling_interval|idle_timeout)
                    result=$(echo "$result" | sed "s/\"$key\":[^,}]*/\"$key\": \"$value\"/")
                    ;;
                log_level|editor|timezone)
                    result=$(echo "$result" | sed "s/\"$key\":[^,}]*/\"$key\": \"$value\"/")
                    ;;
                experimental|parallel)
                    if [[ "$value" == "true" || "$value" == "false" ]]; then
                        result=$(echo "$result" | sed "s/\"$key\":[^,}]*/\"$key\": $value/")
                    fi
                    ;;
            esac
        fi
    done < "$config_file"

    echo "$result"
}

# Get config value with default fallback
# Usage: get_config <key> [default]
# Key format: dot-notation like "limits.global_max_concurrent"
# Example: get_config "limits.global_max_concurrent" 20
get_config() {
    local key="$1"
    local default="${2:-}"
    local value

    if _needle_has_yq; then
        value=$(load_config | yq ".$key" 2>/dev/null)
    else
        # Fallback: simple JSON parsing with jq or grep
        if command -v jq &>/dev/null; then
            value=$(load_config | jq -r ".$key" 2>/dev/null)
        else
            # Very basic fallback - extract from JSON using grep/sed
            value=$(_needle_json_get ".$key")
        fi
    fi

    # Handle null/empty values
    if [[ "$value" == "null" ]] || [[ -z "$value" ]]; then
        echo "$default"
    else
        echo "$value"
    fi
}

# Simple JSON value extraction (fallback without jq/yq)
_needle_json_get() {
    local key="$1"
    local json
    json=$(load_config)

    # Convert dot notation to search pattern
    # This is a very simplified implementation
    local key_name="${key##*.}"

    # Try to extract the value using basic string operations
    # Look for "key": value or "key": "value"
    local pattern="\"$key_name\"[[:space:]]*:[[:space:]]*"
    local match
    match=$(echo "$json" | grep -o "$pattern[^,}]*" | head -1)

    if [[ -n "$match" ]]; then
        # Extract value after colon
        local value="${match#*:}"
        # Trim whitespace
        value="${value#"${value%%[![:space:]]*}"}"
        value="${value%"${value##*[![:space:]]}"}"
        # Remove trailing comma
        value="${value%,}"
        # Remove quotes if present
        value="${value#\"}"
        value="${value%\"}"
        echo "$value"
    else
        echo "null"
    fi
}

# Get config value as integer
# Usage: get_config_int <key> [default]
get_config_int() {
    local value
    value=$(get_config "$1" "$2")
    # Extract numeric part
    echo "${value//[^0-9-]/}"
}

# Get config value as boolean
# Usage: get_config_bool <key> [default]
get_config_bool() {
    local value
    value=$(get_config "$1" "$2")
    case "$value" in
        true|True|TRUE|yes|Yes|YES|1) echo "true" ;;
        false|False|FALSE|no|No|NO|0) echo "false" ;;
        *) echo "${2:-false}" ;;
    esac
}

# Create default configuration file
# Usage: create_default_config [path]
create_default_config() {
    local config_file="${1:-$NEEDLE_CONFIG_FILE}"
    local config_dir
    config_dir=$(dirname "$config_file")

    # Create directory if needed
    if [[ ! -d "$config_dir" ]]; then
        mkdir -p "$config_dir" || {
            _needle_error "Failed to create config directory: $config_dir"
            return 1
        }
    fi

    # Write default config
    cat > "$config_file" << 'EOF'
# NEEDLE Configuration
# See: https://github.com/user/needle#configuration

limits:
  global_max_concurrent: 20
  providers:
    anthropic:
      max_concurrent: 5

runner:
  polling_interval: 2s
  idle_timeout: 300s

strands:
  pluck: true
  explore: true
  mend: true
  weave: false
  unravel: false
  pulse: false
  knot: true

effort:
  budget:
    daily_limit_usd: 50.0
    warning_threshold: 0.8
EOF

    if [[ $? -eq 0 ]]; then
        _needle_success "Created default config: $config_file"
        return 0
    else
        _needle_error "Failed to create config file: $config_file"
        return 1
    fi
}

# Validate configuration
# Usage: validate_config
validate_config() {
    local config_file="${1:-$NEEDLE_CONFIG_FILE}"

    if [[ ! -f "$config_file" ]]; then
        _needle_error "Configuration file not found: $config_file"
        return 1
    fi

    if [[ ! -s "$config_file" ]]; then
        _needle_error "Configuration file is empty: $config_file"
        return 1
    fi

    # If yq is available, validate YAML syntax
    if _needle_has_yq; then
        if ! yq eval '.' "$config_file" &>/dev/null; then
            _needle_error "Invalid YAML syntax in config file: $config_file"
            return 1
        fi
    fi

    # Validate required fields
    local max_concurrent
    max_concurrent=$(get_config "limits.global_max_concurrent" "20")

    if [[ ! "$max_concurrent" =~ ^[0-9]+$ ]] || [[ "$max_concurrent" -lt 1 ]]; then
        _needle_error "Invalid limits.global_max_concurrent: must be positive integer"
        return 1
    fi

    local daily_limit
    daily_limit=$(get_config "effort.budget.daily_limit_usd" "50")

    if [[ ! "$daily_limit" =~ ^[0-9]+\.?[0-9]*$ ]] || [[ "$(echo "$daily_limit < 0" | bc 2>/dev/null || echo 0)" -eq 1 ]]; then
        _needle_error "Invalid effort.budget.daily_limit_usd: must be positive number"
        return 1
    fi

    _needle_debug "Configuration validated successfully"
    return 0
}

# Clear config cache (force reload on next access)
# Usage: clear_config_cache
clear_config_cache() {
    NEEDLE_CONFIG_CACHE=""
}

# Reload configuration from file
# Usage: reload_config
reload_config() {
    clear_config_cache
    load_config
}

# Get config value (simple YAML key extraction) - legacy function
# Usage: _needle_config_get <key>
_needle_config_get() {
    local key="$1"
    local config_file="$NEEDLE_CONFIG_FILE"

    if [[ ! -f "$config_file" ]]; then
        return 1
    fi

    # Simple extraction for top-level keys (no nested objects)
    grep -E "^${key}:" "$config_file" 2>/dev/null | sed 's/^[^:]*: *//' | sed 's/^"//' | sed 's/"$//'
}

# Set config value (simple YAML key setting) - legacy function
# Usage: _needle_config_set <key> <value>
_needle_config_set() {
    local key="$1"
    local value="$2"
    local config_file="$NEEDLE_CONFIG_FILE"

    if [[ ! -f "$config_file" ]]; then
        return 1
    fi

    # Simple replacement for top-level keys
    if grep -q "^${key}:" "$config_file" 2>/dev/null; then
        # Use different separator to avoid issues with paths
        sed -i "s|^${key}:.*|${key}: ${value}|" "$config_file"
    else
        echo "${key}: ${value}" >> "$config_file"
    fi

    # Clear cache after modification
    clear_config_cache
}

# Create default configuration (legacy function wrapper)
_needle_config_create_default() {
    local config_file="${1:-$NEEDLE_CONFIG_FILE}"
    create_default_config "$config_file"
}

# Validate configuration (legacy function wrapper)
_needle_config_validate() {
    local config_file="$NEEDLE_CONFIG_FILE"
    validate_config "$config_file"
}
