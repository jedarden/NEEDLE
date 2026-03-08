#!/usr/bin/env bash
# NEEDLE Agent Adapter Loader Module
# Loads and validates agent YAML configurations
#
# This module implements the agent adapter system that allows:
# - Adding new agents with YAML config only (no code changes)
# - Customizing invocation per agent
# - Different input methods (stdin, file, args)
# - Token extraction patterns

# -----------------------------------------------------------------------------
# Constants
# -----------------------------------------------------------------------------

# Search paths for agent configs (in order of precedence)
NEEDLE_AGENT_PATHS=(
    ".needle/agents"           # Workspace-level configs
    "$NEEDLE_HOME/agents"      # User-level configs
)

# Built-in agents directory (relative to NEEDLE installation)
NEEDLE_BUILTIN_AGENTS_DIR=""  # Set during initialization

# Global associative array for loaded agent
declare -gA NEEDLE_AGENT=()

# -----------------------------------------------------------------------------
# Initialization
# -----------------------------------------------------------------------------

# Initialize the agent loader module
# Sets up paths and ensures dependencies are available
_needle_agent_loader_init() {
    # Determine built-in agents directory
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    NEEDLE_BUILTIN_AGENTS_DIR="$(dirname "$(dirname "$script_dir")")/config/agents"

    # Update NEEDLE_AGENT_PATHS with expanded home directory
    NEEDLE_AGENT_PATHS=(
        ".needle/agents"
        "${NEEDLE_HOME}/agents"
        "$NEEDLE_BUILTIN_AGENTS_DIR"
    )
}

# -----------------------------------------------------------------------------
# YAML Parsing
# -----------------------------------------------------------------------------

# Parse YAML file using Python (with PyYAML)
# Usage: _needle_parse_yaml <file> <path>
# Example: _needle_parse_yaml agent.yaml '.name'
# Example: _needle_parse_yaml agent.yaml '.input.method'
_needle_parse_yaml_python() {
    local file="$1"
    local path="$2"

    python3 -c "
import yaml
import sys

def get_value(data, path):
    '''Get value from nested dict using dot-notation path'''
    if not path or path == '.':
        return data

    keys = path.strip('.').split('.')
    current = data

    for key in keys:
        if not key:
            continue
        if isinstance(current, dict) and key in current:
            current = current[key]
        elif isinstance(current, list):
            try:
                idx = int(key)
                current = current[idx]
            except (ValueError, IndexError):
                return None
        else:
            return None

    return current

try:
    with open('$file', 'r') as f:
        data = yaml.safe_load(f)

    value = get_value(data, '$path')

    if value is None:
        sys.exit(1)

    # Handle different types
    if isinstance(value, bool):
        print('true' if value else 'false')
    elif isinstance(value, list):
        # Output list as JSON-like format for parsing
        import json
        print(json.dumps(value))
    elif isinstance(value, dict):
        import json
        print(json.dumps(value))
    else:
        print(value)

except FileNotFoundError:
    print('Error: File not found: $file', file=sys.stderr)
    sys.exit(1)
except yaml.YAMLError as e:
    print(f'Error: Invalid YAML: {e}', file=sys.stderr)
    sys.exit(1)
except Exception as e:
    print(f'Error: {e}', file=sys.stderr)
    sys.exit(1)
" 2>/dev/null
}

# Parse YAML file using yq (if available)
# Usage: _needle_parse_yaml_yq <file> <path>
_needle_parse_yaml_yq() {
    local file="$1"
    local path="$2"

    if ! command -v yq &>/dev/null; then
        return 1
    fi

    yq "$path" "$file" 2>/dev/null
}

# Parse YAML file with fallback
# Tries yq first, falls back to Python
# Usage: _needle_parse_yaml <file> <path>
_needle_parse_yaml() {
    local file="$1"
    local path="$2"

    # Ensure file exists
    if [[ ! -f "$file" ]]; then
        _needle_error "YAML file not found: $file"
        return 1
    fi

    # Try yq first (faster), fall back to Python
    if command -v yq &>/dev/null; then
        local result
        result=$(_needle_parse_yaml_yq "$file" "$path")
        if [[ $? -eq 0 && -n "$result" && "$result" != "null" ]]; then
            echo "$result"
            return 0
        fi
    fi

    # Fall back to Python with PyYAML
    _needle_parse_yaml_python "$file" "$path"
    return $?
}

# Parse a list field from YAML
# Returns items as newline-separated values
# Usage: _needle_parse_yaml_list <file> <path>
_needle_parse_yaml_list() {
    local file="$1"
    local path="$2"

    local result
    result=$(_needle_parse_yaml "$file" "$path")

    if [[ $? -ne 0 ]]; then
        return 1
    fi

    # Parse JSON array format
    if [[ "$result" == "["*"]" ]]; then
        # Remove brackets and parse
        echo "$result" | python3 -c "
import sys
import json
data = json.load(sys.stdin)
for item in data:
    print(item)
" 2>/dev/null
    else
        # Single value or already formatted
        echo "$result"
    fi
}

# -----------------------------------------------------------------------------
# Agent Discovery
# -----------------------------------------------------------------------------

# Find agent config file by name
# Searches in order: workspace → user → builtin
# Usage: _needle_find_agent_config <agent_name>
# Returns: path to agent config file, or empty if not found
_needle_find_agent_config() {
    local agent_name="$1"
    local agent_file

    # Initialize module if not done
    [[ -z "$NEEDLE_BUILTIN_AGENTS_DIR" ]] && _needle_agent_loader_init

    # Search in order of precedence
    for dir in "${NEEDLE_AGENT_PATHS[@]}"; do
        # Handle relative vs absolute paths
        local search_dir="$dir"
        if [[ "$dir" != /* ]]; then
            # Relative path - use from current directory
            search_dir="$(pwd)/$dir"
        fi

        agent_file="$search_dir/${agent_name}.yaml"
        if [[ -f "$agent_file" ]]; then
            echo "$agent_file"
            return 0
        fi

        # Also try .yml extension
        agent_file="$search_dir/${agent_name}.yml"
        if [[ -f "$agent_file" ]]; then
            echo "$agent_file"
            return 0
        fi
    done

    return 1
}

# List all available agents
# Usage: _needle_list_available_agents [--json]
_needle_list_available_agents() {
    local json_output=false
    [[ "${1:-}" == "--json" ]] && json_output=true

    # Initialize module if not done
    [[ -z "$NEEDLE_BUILTIN_AGENTS_DIR" ]] && _needle_agent_loader_init

    local -a agents=()
    local -A seen=()

    # Collect all unique agent names
    for dir in "${NEEDLE_AGENT_PATHS[@]}"; do
        local search_dir="$dir"
        if [[ "$dir" != /* ]]; then
            search_dir="$(pwd)/$dir"
        fi

        if [[ -d "$search_dir" ]]; then
            for file in "$search_dir"/*.yaml "$search_dir"/*.yml; do
                [[ -f "$file" ]] || continue
                local basename
                basename=$(basename "$file")
                basename="${basename%.yaml}"
                basename="${basename%.yml}"

                if [[ -z "${seen[$basename]:-}" ]]; then
                    seen[$basename]=1
                    agents+=("$basename")
                fi
            done 2>/dev/null
        fi
    done

    if [[ "$json_output" == "true" ]]; then
        local json="["
        local first=true
        for agent in "${agents[@]}"; do
            if [[ "$first" == "true" ]]; then
                first=false
            else
                json+=","
            fi
            json+="\"$(_needle_json_escape "$agent")\""
        done
        json+="]"
        echo "$json"
    else
        printf '%s\n' "${agents[@]}"
    fi
}

# -----------------------------------------------------------------------------
# Agent Loading
# -----------------------------------------------------------------------------

# Load agent configuration into NEEDLE_AGENT associative array
# Usage: _needle_load_agent <agent_name>
# Populates: NEEDLE_AGENT associative array
_needle_load_agent() {
    local agent_name="$1"

    # Initialize module if not done
    [[ -z "$NEEDLE_BUILTIN_AGENTS_DIR" ]] && _needle_agent_loader_init

    # Find agent config file
    local agent_file
    agent_file=$(_needle_find_agent_config "$agent_name")

    if [[ -z "$agent_file" ]]; then
        _needle_error "Agent not found: $agent_name"
        _needle_info "Searched in: ${NEEDLE_AGENT_PATHS[*]}"
        return 1
    fi

    _needle_debug "Loading agent from: $agent_file"

    # Clear previous config
    NEEDLE_AGENT=()

    # Load all properties
    NEEDLE_AGENT[name]=$(_needle_parse_yaml "$agent_file" '.name' 2>/dev/null)
    NEEDLE_AGENT[description]=$(_needle_parse_yaml "$agent_file" '.description' 2>/dev/null)
    NEEDLE_AGENT[version]=$(_needle_parse_yaml "$agent_file" '.version' 2>/dev/null)
    NEEDLE_AGENT[runner]=$(_needle_parse_yaml "$agent_file" '.runner' 2>/dev/null)
    NEEDLE_AGENT[provider]=$(_needle_parse_yaml "$agent_file" '.provider' 2>/dev/null)
    NEEDLE_AGENT[model]=$(_needle_parse_yaml "$agent_file" '.model' 2>/dev/null)
    NEEDLE_AGENT[invoke]=$(_needle_parse_yaml "$agent_file" '.invoke' 2>/dev/null)

    # Input configuration
    NEEDLE_AGENT[input_method]=$(_needle_parse_yaml "$agent_file" '.input.method' 2>/dev/null)
    NEEDLE_AGENT[input_file_path]=$(_needle_parse_yaml "$agent_file" '.input.file_path' 2>/dev/null)
    NEEDLE_AGENT[input_arg_flag]=$(_needle_parse_yaml "$agent_file" '.input.arg_flag' 2>/dev/null)

    # Output configuration
    NEEDLE_AGENT[output_format]=$(_needle_parse_yaml "$agent_file" '.output.format' 2>/dev/null)
    NEEDLE_AGENT[token_pattern]=$(_needle_parse_yaml "$agent_file" '.output.token_pattern' 2>/dev/null)

    # Parse success/retry/fail codes as newline-separated values
    NEEDLE_AGENT[success_codes]=$(_needle_parse_yaml_list "$agent_file" '.output.success_codes' 2>/dev/null | tr '\n' ' ')
    NEEDLE_AGENT[retry_codes]=$(_needle_parse_yaml_list "$agent_file" '.output.retry_codes' 2>/dev/null | tr '\n' ' ')
    NEEDLE_AGENT[fail_codes]=$(_needle_parse_yaml_list "$agent_file" '.output.fail_codes' 2>/dev/null | tr '\n' ' ')

    # Rate limits
    NEEDLE_AGENT[requests_per_minute]=$(_needle_parse_yaml "$agent_file" '.limits.requests_per_minute' 2>/dev/null)
    NEEDLE_AGENT[max_concurrent]=$(_needle_parse_yaml "$agent_file" '.limits.max_concurrent' 2>/dev/null)

    # Store file path and directory for reference
    NEEDLE_AGENT[_file]="$agent_file"
    NEEDLE_AGENT[agent_dir]="$(dirname "$agent_file")"

    # Set defaults for missing values
    [[ -z "${NEEDLE_AGENT[name]:-}" ]] && NEEDLE_AGENT[name]="$agent_name"
    [[ -z "${NEEDLE_AGENT[input_method]:-}" ]] && NEEDLE_AGENT[input_method]="heredoc"
    [[ -z "${NEEDLE_AGENT[output_format]:-}" ]] && NEEDLE_AGENT[output_format]="text"
    [[ -z "${NEEDLE_AGENT[success_codes]:-}" ]] && NEEDLE_AGENT[success_codes]="0"
    [[ -z "${NEEDLE_AGENT[requests_per_minute]:-}" ]] && NEEDLE_AGENT[requests_per_minute]="60"
    [[ -z "${NEEDLE_AGENT[max_concurrent]:-}" ]] && NEEDLE_AGENT[max_concurrent]="5"

    return 0
}

# -----------------------------------------------------------------------------
# Agent Validation
# -----------------------------------------------------------------------------

# Validate loaded agent configuration
# Usage: _needle_validate_agent <agent_name>
# Returns: 0 if valid, 1 if invalid
_needle_validate_agent() {
    local agent_name="$1"

    # Load agent if not already loaded or if different agent
    if [[ "${NEEDLE_AGENT[name]:-}" != "$agent_name" ]]; then
        _needle_load_agent "$agent_name" || return 1
    fi

    local errors=0

    # Check runner exists
    if [[ -z "${NEEDLE_AGENT[runner]:-}" ]]; then
        _needle_error "Missing required field: runner"
        ((errors++))
    else
        if ! command -v "${NEEDLE_AGENT[runner]}" &>/dev/null; then
            _needle_error "Runner not found in PATH: ${NEEDLE_AGENT[runner]}"
            ((errors++))
        fi
    fi

    # Validate invoke template
    if [[ -z "${NEEDLE_AGENT[invoke]:-}" ]]; then
        _needle_error "Missing required field: invoke template"
        ((errors++))
    fi

    # Validate input method
    local valid_methods="heredoc stdin file args"
    if [[ -n "${NEEDLE_AGENT[input_method]:-}" ]]; then
        if [[ ! " $valid_methods " =~ " ${NEEDLE_AGENT[input_method]} " ]]; then
            _needle_error "Invalid input method: ${NEEDLE_AGENT[input_method]} (valid: $valid_methods)"
            ((errors++))
        fi
    fi

    # Validate output format
    local valid_formats="json text structured stream-json"
    if [[ -n "${NEEDLE_AGENT[output_format]:-}" ]]; then
        if [[ ! " $valid_formats " =~ " ${NEEDLE_AGENT[output_format]} " ]]; then
            _needle_error "Invalid output format: ${NEEDLE_AGENT[output_format]} (valid: $valid_formats)"
            ((errors++))
        fi
    fi

    # Validate provider
    if [[ -z "${NEEDLE_AGENT[provider]:-}" ]]; then
        _needle_warn "Missing optional field: provider (rate limiting may not work)"
    fi

    # Warn about missing token pattern for text output
    if [[ "${NEEDLE_AGENT[output_format]}" == "text" && -z "${NEEDLE_AGENT[token_pattern]:-}" ]]; then
        _needle_debug "No token_pattern defined for text output - token tracking unavailable"
    fi

    [[ $errors -eq 0 ]]
}

# Quick check if agent is ready (installed and valid config)
# Usage: _needle_is_agent_configured <agent_name>
# Returns: 0 if ready, 1 otherwise
_needle_is_agent_configured() {
    local agent_name="$1"

    # Try to load and validate
    _needle_load_agent "$agent_name" &>/dev/null || return 1
    _needle_validate_agent "$agent_name" &>/dev/null || return 1

    return 0
}

# -----------------------------------------------------------------------------
# Agent Information Display
# -----------------------------------------------------------------------------

# Display loaded agent configuration
# Usage: _needle_show_agent_config
_needle_show_agent_config() {
    if [[ ${#NEEDLE_AGENT[@]} -eq 0 ]]; then
        _needle_warn "No agent loaded"
        return 1
    fi

    _needle_header "Agent: ${NEEDLE_AGENT[name]}"

    _needle_table_row "Description" "${NEEDLE_AGENT[description]:-}"
    _needle_table_row "Version" "${NEEDLE_AGENT[version]:-}"
    _needle_table_row "Runner" "${NEEDLE_AGENT[runner]:-}"
    _needle_table_row "Provider" "${NEEDLE_AGENT[provider]:-}"
    _needle_table_row "Model" "${NEEDLE_AGENT[model]:-}"

    _needle_print ""
    _needle_section "Input"
    _needle_table_row "Method" "${NEEDLE_AGENT[input_method]:-}"
    [[ -n "${NEEDLE_AGENT[input_file_path]:-}" ]] && _needle_table_row "File Path" "${NEEDLE_AGENT[input_file_path]}"
    [[ -n "${NEEDLE_AGENT[input_arg_flag]:-}" ]] && _needle_table_row "Arg Flag" "${NEEDLE_AGENT[input_arg_flag]}"

    _needle_print ""
    _needle_section "Output"
    _needle_table_row "Format" "${NEEDLE_AGENT[output_format]:-}"
    [[ -n "${NEEDLE_AGENT[token_pattern]:-}" ]] && _needle_table_row "Token Pattern" "${NEEDLE_AGENT[token_pattern]}"

    _needle_print ""
    _needle_section "Rate Limits"
    _needle_table_row "Requests/Min" "${NEEDLE_AGENT[requests_per_minute]:-}"
    _needle_table_row "Max Concurrent" "${NEEDLE_AGENT[max_concurrent]:-}"

    _needle_print ""
    _needle_section "Invoke Template"
    _needle_print "${NEEDLE_AGENT[invoke]}"
}

# Get agent property value
# Usage: _needle_get_agent_property <property_name>
# Example: _needle_get_agent_property runner
_needle_get_agent_property() {
    local property="$1"
    echo "${NEEDLE_AGENT[$property]:-}"
}

# Export agent config as JSON
# Usage: _needle_export_agent_json
_needle_export_agent_json() {
    if [[ ${#NEEDLE_AGENT[@]} -eq 0 ]]; then
        echo "{}"
        return 1
    fi

    local json="{"
    local first=true

    for key in "${!NEEDLE_AGENT[@]}"; do
        # Skip internal keys
        [[ "$key" == _* ]] && continue

        if [[ "$first" == "true" ]]; then
            first=false
        else
            json+=","
        fi

        json+="\"$(_needle_json_escape "$key")\":\"$(_needle_json_escape "${NEEDLE_AGENT[$key]}")\""
    done

    json+="}"
    echo "$json"
}

# Initialize module on load
_needle_agent_loader_init
