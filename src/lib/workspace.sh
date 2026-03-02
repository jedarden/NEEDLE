#!/usr/bin/env bash
# NEEDLE CLI Workspace Configuration Loader
# Load, merge, and cache workspace-level configuration with global config

# Workspace config cache (associative array for multiple workspaces)
declare -A _NEEDLE_WORKSPACE_CACHE

# Find workspace root directory (where .needle.yaml might be)
# Looks for .needle.yaml starting from given directory and walking up to /
# Usage: _needle_find_workspace_root [start_dir]
# Returns: path to workspace root or empty string
_needle_find_workspace_root() {
    local start_dir="${1:-$(pwd)}"
    local current_dir

    # Resolve to absolute path
    current_dir="$(cd "$start_dir" 2>/dev/null && pwd)" || return 1

    # Walk up directory tree looking for .needle.yaml
    while [[ -n "$current_dir" ]] && [[ "$current_dir" != "/" ]]; do
        if [[ -f "$current_dir/.needle.yaml" ]]; then
            echo "$current_dir"
            return 0
        fi

        # Move to parent directory
        current_dir="$(dirname "$current_dir")"
    done

    # No workspace config found
    return 1
}

# Check if yq is available
_needle_workspace_has_yq() {
    command -v yq &>/dev/null
}

# Merge two YAML configs using Python
# Usage: _needle_merge_yaml_python <base_file> <override_file>
# Returns: merged YAML content
_needle_merge_yaml_python() {
    local base_file="$1"
    local override_file="$2"

    python3 -c "
import yaml
import sys

def deep_merge(base, override):
    '''Deep merge two dictionaries, with override taking precedence'''
    if not isinstance(base, dict) or not isinstance(override, dict):
        return override

    result = base.copy()
    for key, value in override.items():
        if key in result and isinstance(result[key], dict) and isinstance(value, dict):
            result[key] = deep_merge(result[key], value)
        else:
            result[key] = value
    return result

try:
    with open('$base_file', 'r') as f:
        base = yaml.safe_load(f) or {}

    with open('$override_file', 'r') as f:
        override = yaml.safe_load(f) or {}

    merged = deep_merge(base, override)
    yaml.dump(merged, sys.stdout, default_flow_style=False, sort_keys=False)
except Exception as e:
    print(f'Error merging configs: {e}', file=sys.stderr)
    sys.exit(1)
" 2>/dev/null
}

# Parse YAML using Python
# Usage: _needle_parse_workspace_yaml_python <content> <path>
_needle_parse_workspace_yaml_python() {
    local content="$1"
    local path="$2"

    echo "$content" | python3 -c "
import yaml
import sys
import json

def get_value(data, path):
    '''Get value from nested dict using dot-notation path'''
    if not path or path == '.':
        return data

    # Convert yq-style path to keys
    keys = path.lstrip('.').split('.')
    current = data

    for key in keys:
        if current is None:
            return None
        if isinstance(current, dict) and key in current:
            current = current[key]
        else:
            return None

    return current

try:
    data = yaml.safe_load(sys.stdin)
    if data is None:
        print('null')
        sys.exit(0)

    value = get_value(data, '$path')

    if value is None:
        print('null')
    elif isinstance(value, (dict, list)):
        print(json.dumps(value))
    elif isinstance(value, bool):
        print('true' if value else 'false')
    else:
        print(value)
except Exception as e:
    print('null', file=sys.stdout)
    sys.exit(1)
" 2>/dev/null
}

# Load and merge workspace configuration with global config
# Workspace config overrides global settings
# Usage: load_workspace_config [workspace_path]
# Returns: merged configuration in YAML format
load_workspace_config() {
    local workspace="${1:-$(pwd)}"
    local ws_config
    local global_config="${NEEDLE_HOME:-$HOME/.needle}/config.yaml"
    local cache_key
    local merged_config

    # Resolve workspace to absolute path
    workspace="$(cd "$workspace" 2>/dev/null && pwd)" || {
        _needle_error "Invalid workspace path: $1"
        return 1
    }

    # Create cache key from workspace path
    cache_key="$workspace"

    # Check cache first
    if [[ -n "${_NEEDLE_WORKSPACE_CACHE[$cache_key]:-}" ]]; then
        echo "${_NEEDLE_WORKSPACE_CACHE[$cache_key]}"
        return 0
    fi

    # Find workspace config (look for .needle.yaml in workspace root)
    ws_config="$workspace/.needle.yaml"

    # If no workspace config, try to find it by walking up
    if [[ ! -f "$ws_config" ]]; then
        local found_root
        found_root=$(_needle_find_workspace_root "$workspace" 2>/dev/null)
        if [[ -n "$found_root" ]]; then
            ws_config="$found_root/.needle.yaml"
        fi
    fi

    # Check if global config exists
    if [[ ! -f "$global_config" ]]; then
        # No global config
        if [[ -f "$ws_config" ]]; then
            # Use workspace config only
            merged_config=$(cat "$ws_config")
        else
            # No config at all, return defaults
            merged_config="$_NEEDLE_CONFIG_DEFAULTS"
        fi
    else
        # Global config exists
        if [[ ! -f "$ws_config" ]]; then
            # No workspace config, use global
            merged_config=$(cat "$global_config")
        else
            # Both configs exist - merge them
            # Use Python for merging (more reliable than yq)
            merged_config=$(_needle_merge_yaml_python "$global_config" "$ws_config" 2>/dev/null)
            if [[ $? -ne 0 ]] || [[ -z "$merged_config" ]]; then
                _needle_warn "Workspace config merge failed, using workspace config"
                merged_config=$(cat "$ws_config")
            fi
        fi
    fi

    # Cache the result
    _NEEDLE_WORKSPACE_CACHE[$cache_key]="$merged_config"

    echo "$merged_config"
}

# Get a specific setting from workspace configuration
# Falls back to default value if key not found
# Usage: get_workspace_setting <workspace> <key> [default]
# Example: get_workspace_setting "/home/user/project" "limits.max_concurrent" "10"
get_workspace_setting() {
    local workspace="$1"
    local key="$2"
    local default="${3:-}"
    local config
    local value

    # Load merged config
    config=$(load_workspace_config "$workspace") || {
        echo "$default"
        return 0
    }

    # Extract value using Python YAML parser
    if command -v python3 &>/dev/null; then
        value=$(_needle_parse_workspace_yaml_python "$config" "$key" 2>/dev/null)
    elif _needle_workspace_has_yq; then
        value=$(echo "$config" | yq ".$key" 2>/dev/null)
    elif command -v jq &>/dev/null; then
        # Try to parse as JSON
        value=$(echo "$config" | jq -r ".$key" 2>/dev/null)
    else
        # Basic fallback extraction
        value=$(_needle_config_extract_value "$config" "$key")
    fi

    # Handle null/empty values - return default
    if [[ "$value" == "null" ]] || [[ -z "$value" ]]; then
        echo "$default"
    else
        echo "$value"
    fi
}

# Get workspace setting as integer
# Usage: get_workspace_setting_int <workspace> <key> [default]
get_workspace_setting_int() {
    local value
    value=$(get_workspace_setting "$1" "$2" "$3")
    # Extract numeric part only
    echo "${value//[^0-9-]/}"
}

# Get workspace setting as boolean
# Usage: get_workspace_setting_bool <workspace> <key> [default]
get_workspace_setting_bool() {
    local value
    value=$(get_workspace_setting "$1" "$2" "$3")
    case "$value" in
        true|True|TRUE|yes|Yes|YES|1) echo "true" ;;
        false|False|FALSE|no|No|NO|0) echo "false" ;;
        *) echo "${3:-false}" ;;
    esac
}

# Basic value extraction fallback (without Python/yq/jq)
# Usage: _needle_config_extract_value <config_content> <key>
# Key format: dot.notation like "limits.max_concurrent"
_needle_config_extract_value() {
    local config="$1"
    local key="$2"
    local key_parts
    local current_value="$config"
    local part

    # Split key by dots
    IFS='.' read -ra key_parts <<< "$key"

    # Navigate through nested structure (simplified YAML)
    for part in "${key_parts[@]}"; do
        # Try to find the key in current content
        # Look for patterns like "key: value"
        local pattern="^[[:space:]]*${part}[[:space:]]*:[[:space:]]*"
        local match

        match=$(echo "$current_value" | grep -E "$pattern" | head -1)

        if [[ -n "$match" ]]; then
            # Extract value after colon
            current_value="${match#*:}"
            # Trim whitespace
            current_value="${current_value#"${current_value%%[![:space:]]*}"}"
            current_value="${current_value%"${current_value##*[![:space:]]}"}"
            # Remove trailing comma
            current_value="${current_value%,}"
            # Remove quotes if present
            current_value="${current_value#\"}"
            current_value="${current_value%\"}"
        else
            echo "null"
            return 1
        fi
    done

    echo "$current_value"
}

# Check if workspace has a local configuration file
# Usage: has_workspace_config [workspace_path]
# Returns: 0 if workspace config exists, 1 otherwise
has_workspace_config() {
    local workspace="${1:-$(pwd)}"
    local ws_config

    # Resolve to absolute path
    workspace="$(cd "$workspace" 2>/dev/null && pwd)" || return 1

    # Check for .needle.yaml directly
    ws_config="$workspace/.needle.yaml"
    if [[ -f "$ws_config" ]]; then
        return 0
    fi

    # Try walking up to find it
    local found_root
    found_root=$(_needle_find_workspace_root "$workspace" 2>/dev/null)
    [[ -n "$found_root" ]]
}

# Get the path to the workspace config file
# Usage: get_workspace_config_path [workspace_path]
# Returns: path to .needle.yaml or empty string
get_workspace_config_path() {
    local workspace="${1:-$(pwd)}"
    local ws_config

    # Resolve to absolute path
    workspace="$(cd "$workspace" 2>/dev/null && pwd)" || return 1

    # Check for .needle.yaml directly
    ws_config="$workspace/.needle.yaml"
    if [[ -f "$ws_config" ]]; then
        echo "$ws_config"
        return 0
    fi

    # Try walking up to find it
    local found_root
    found_root=$(_needle_find_workspace_root "$workspace" 2>/dev/null)
    if [[ -n "$found_root" ]]; then
        echo "$found_root/.needle.yaml"
        return 0
    fi

    return 1
}

# Clear workspace config cache for a specific workspace or all
# Usage: clear_workspace_cache [workspace_path]
clear_workspace_cache() {
    local workspace="${1:-}"

    if [[ -n "$workspace" ]]; then
        local cache_key
        workspace="$(cd "$workspace" 2>/dev/null && pwd)" || return 1
        cache_key="$workspace"
        unset "_NEEDLE_WORKSPACE_CACHE[$cache_key]"
    else
        # Clear all cached workspace configs
        _NEEDLE_WORKSPACE_CACHE=()
    fi
}

# Reload workspace configuration (clear cache and reload)
# Usage: reload_workspace_config [workspace_path]
reload_workspace_config() {
    local workspace="${1:-$(pwd)}"
    clear_workspace_cache "$workspace"
    load_workspace_config "$workspace"
}

# List all cached workspaces
# Usage: list_cached_workspaces
list_cached_workspaces() {
    local workspace
    for workspace in "${!_NEEDLE_WORKSPACE_CACHE[@]}"; do
        echo "$workspace"
    done
}
