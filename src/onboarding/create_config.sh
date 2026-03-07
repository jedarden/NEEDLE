#!/usr/bin/env bash
# NEEDLE CLI Config Creation Module
# Generate default ~/.needle/config.yaml during onboarding

# -----------------------------------------------------------------------------
# Default Configuration Values
# -----------------------------------------------------------------------------

# Default config version
NEEDLE_CONFIG_VERSION=1

# Default worker settings
NEEDLE_DEFAULT_MAX_CONCURRENT=5
NEEDLE_DEFAULT_AGENT="claude"

# Default telemetry settings
NEEDLE_DEFAULT_TELEMETRY_ENABLED=true
NEEDLE_DEFAULT_LOG_DIR="~/.needle/logs"

# Default budget settings
NEEDLE_DEFAULT_DAILY_LIMIT=10.00
NEEDLE_DEFAULT_WARN_THRESHOLD=0.8

# -----------------------------------------------------------------------------
# Directory Structure Creation
# -----------------------------------------------------------------------------

# Create the NEEDLE home directory structure
# Usage: _needle_create_config_dirs [home_path]
# Returns: 0 on success, 1 on failure
_needle_create_config_dirs() {
    local home_path="${1:-$NEEDLE_HOME}"

    # Expand ~ if present
    home_path="${home_path//\~/$HOME}"

    _needle_verbose "Creating directory structure at: $home_path"

    # Create main directories
    local dirs=(
        "$home_path"
        "$home_path/state"
        "$home_path/cache"
        "$home_path/logs"
        "$home_path/hooks"
    )

    for dir in "${dirs[@]}"; do
        if [[ ! -d "$dir" ]]; then
            if ! mkdir -p "$dir" 2>/dev/null; then
                _needle_error "Failed to create directory: $dir"
                return 1
            fi
            _needle_debug "Created: $dir"
        fi
    done

    _needle_success "Created directory structure"
    return 0
}

# -----------------------------------------------------------------------------
# Interactive Prompts
# -----------------------------------------------------------------------------

# Prompt for max concurrent workers
# Usage: _needle_prompt_max_workers [default]
# Returns: User input or default value
_needle_prompt_max_workers() {
    local default="${1:-$NEEDLE_DEFAULT_MAX_CONCURRENT}"

    if ! _needle_is_interactive; then
        echo "$default"
        return 0
    fi

    _needle_print ""
    _needle_print_color "$NEEDLE_COLOR_CYAN" "? Maximum concurrent workers"
    _needle_print "  This controls how many agents can run in parallel."
    _needle_print -n "  [$default]: "

    local response
    read -r response

    # Validate input
    if [[ -z "$response" ]]; then
        echo "$default"
        return 0
    fi

    if [[ "$response" =~ ^[0-9]+$ ]] && [[ "$response" -ge 1 ]] && [[ "$response" -le 100 ]]; then
        echo "$response"
        return 0
    fi

    _needle_warn "Invalid input, using default: $default"
    echo "$default"
}

# Prompt for default agent
# Usage: _needle_prompt_default_agent [default]
# Returns: User input or default value
_needle_prompt_default_agent() {
    local default="${1:-$NEEDLE_DEFAULT_AGENT}"

    if ! _needle_is_interactive; then
        echo "$default"
        return 0
    fi

    # Get list of installed agents
    local installed_agents
    installed_agents=$(_needle_get_installed_agents 2>/dev/null || echo "")

    # If only one agent is installed, use it as default
    if [[ -n "$installed_agents" ]] && [[ "$(echo "$installed_agents" | wc -w)" -eq 1 ]]; then
        default="$installed_agents"
    fi

    _needle_print ""
    _needle_print_color "$NEEDLE_COLOR_CYAN" "? Default agent"
    _needle_print "  Available agents: claude, opencode, codex, aider"

    if [[ -n "$installed_agents" ]]; then
        _needle_print "  Installed: $installed_agents"
    fi

    _needle_print -n "  [$default]: "

    local response
    read -r response

    if [[ -z "$response" ]]; then
        echo "$default"
        return 0
    fi

    # Validate agent name
    case "$response" in
        claude|opencode|codex|aider)
            echo "$response"
            ;;
        *)
            _needle_warn "Unknown agent '$response', using default: $default"
            echo "$default"
            ;;
    esac
}

# Prompt for telemetry preference
# Usage: _needle_prompt_telemetry [default]
# Returns: "true" or "false"
_needle_prompt_telemetry() {
    local default="${1:-$NEEDLE_DEFAULT_TELEMETRY_ENABLED}"

    if ! _needle_is_interactive; then
        echo "$default"
        return 0
    fi

    local default_prompt="n"
    [[ "$default" == "true" ]] && default_prompt="y"

    if _needle_confirm "Enable telemetry and logging?" "$default_prompt"; then
        echo "true"
    else
        echo "false"
    fi
}

# Prompt for daily budget limit
# Usage: _needle_prompt_daily_limit [default]
# Returns: Budget value as decimal string
_needle_prompt_daily_limit() {
    local default="${1:-$NEEDLE_DEFAULT_DAILY_LIMIT}"

    if ! _needle_is_interactive; then
        echo "$default"
        return 0
    fi

    _needle_print ""
    _needle_print_color "$NEEDLE_COLOR_CYAN" "? Daily budget limit (USD)"
    _needle_print "  Maximum daily spend on API calls."
    _needle_print -n "  [$default]: "

    local response
    read -r response

    if [[ -z "$response" ]]; then
        echo "$default"
        return 0
    fi

    # Validate numeric input
    if [[ "$response" =~ ^[0-9]+\.?[0-9]*$ ]]; then
        echo "$response"
        return 0
    fi

    _needle_warn "Invalid input, using default: $default"
    echo "$default"
}

# -----------------------------------------------------------------------------
# Config Generation
# -----------------------------------------------------------------------------

# Generate YAML config content
# Usage: _needle_generate_config_yaml [options]
# Options:
#   --max-workers N       Max concurrent workers (default: 5)
#   --default-agent AGENT Default agent (default: claude)
#   --telemetry BOOL      Enable telemetry (default: true)
#   --daily-limit N       Daily budget limit (default: 10.00)
#   --warn-threshold N    Warning threshold (default: 0.8)
# Returns: YAML config content
_needle_generate_config_yaml() {
    local max_workers="$NEEDLE_DEFAULT_MAX_CONCURRENT"
    local default_agent="$NEEDLE_DEFAULT_AGENT"
    local telemetry="$NEEDLE_DEFAULT_TELEMETRY_ENABLED"
    local daily_limit="$NEEDLE_DEFAULT_DAILY_LIMIT"
    local warn_threshold="$NEEDLE_DEFAULT_WARN_THRESHOLD"

    # Parse options
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --max-workers)
                max_workers="$2"
                shift 2
                ;;
            --default-agent)
                default_agent="$2"
                shift 2
                ;;
            --telemetry)
                telemetry="$2"
                shift 2
                ;;
            --daily-limit)
                daily_limit="$2"
                shift 2
                ;;
            --warn-threshold)
                warn_threshold="$2"
                shift 2
                ;;
            *)
                shift
                ;;
        esac
    done

    # Convert telemetry to YAML boolean
    local telemetry_yaml="true"
    [[ "$telemetry" != "true" ]] && telemetry_yaml="false"

    cat << EOF
# NEEDLE Configuration
# Generated by needle init
# See: https://github.com/user/needle#configuration

needle:
  version: $NEEDLE_CONFIG_VERSION

workers:
  max_concurrent: $max_workers
  default_agent: $default_agent

telemetry:
  enabled: $telemetry_yaml
  log_dir: ~/.needle/logs

budget:
  daily_limit_usd: $daily_limit
  warn_threshold: $warn_threshold

# Billing model configuration
# Controls how aggressively NEEDLE uses API budget
billing:
  # model: Billing model profile
  #   - pay_per_token: Conservative (default), minimize token usage
  #   - use_or_lose: Aggressive, use allocated budget
  #   - unlimited: Maximum throughput, no budget enforcement
  model: pay_per_token
  # daily_budget_usd: Daily budget in USD (used by use_or_lose model)
  daily_budget_usd: $daily_limit

# Advanced settings (edit manually for customization)
limits:
  global_max_concurrent: 20
  providers:
    anthropic:
      max_concurrent: 5
      requests_per_minute: 60

runner:
  polling_interval: 2s
  idle_timeout: 300s

# Strand configuration
# Values: true (always enabled), false (always disabled), auto (follows billing model)
strands:
  pluck: auto    # Primary work from configured workspaces
  explore: auto  # Look for work in other workspaces
  mend: auto     # Maintenance and cleanup
  weave: auto    # Create beads from documentation gaps
  unravel: auto  # Create alternatives for blocked beads
  pulse: auto    # Codebase health monitoring
  knot: auto     # Alert human when stuck

mend:
  heartbeat_max_age: 3600
  max_log_files: 100
  min_interval: 60

effort:
  budget:
    daily_limit_usd: $daily_limit
    warning_threshold: $warn_threshold

hooks:
  timeout: 30s
  fail_action: warn

# File collision management
# Controls how NEEDLE handles concurrent file edits across workers
file_locks:
  # timeout: Maximum time a file lock can be held before considered stale
  # Supports: 30m (30 minutes), 1h (1 hour), 3600s (3600 seconds)
  timeout: 30m
  # stale_action: What to do when a lock is held too long
  #   warn    - Log warning but keep lock (default)
  #   release - Force release the stale lock
  #   ignore  - Don't check for stale locks
  stale_action: warn

mitosis:
  enabled: true
  skip_types: bug,hotfix,incident
  skip_labels: no-mitosis,atomic,single-task
  max_children: 5
  min_children: 2
  min_complexity: 100
  timeout: 60

knot:
  rate_limit_interval: 3600

watchdog:
  interval: 30
  heartbeat_timeout: 120
  bead_timeout: 600
  recovery_action: restart
EOF
}

# Create config file with defaults
# Usage: _needle_create_default_config [options]
# Options:
#   --force               Overwrite existing config
#   --defaults            Use all default values (non-interactive)
#   --path PATH           Custom config path
# Returns: 0 on success, 1 on failure
_needle_create_default_config() {
    local force=false
    local use_defaults=false
    local config_path=""

    # Parse options
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --force)
                force=true
                shift
                ;;
            --defaults)
                use_defaults=true
                shift
                ;;
            --path)
                config_path="$2"
                shift 2
                ;;
            *)
                shift
                ;;
        esac
    done

    # Determine config path
    local home_path="${NEEDLE_HOME:-$HOME/.needle}"
    home_path="${home_path//\~/$HOME}"

    if [[ -z "$config_path" ]]; then
        config_path="$home_path/config.yaml"
    fi

    # Expand path
    config_path="${config_path//\~/$HOME}"

    # Check if config exists
    if [[ -f "$config_path" ]] && [[ "$force" != "true" ]]; then
        _needle_warn "Config already exists: $config_path"
        _needle_info "Use --force to overwrite"
        return 0
    fi

    # Create directory structure
    if ! _needle_create_config_dirs "$home_path"; then
        return 1
    fi

    # Ensure parent directory of config path exists (for custom paths)
    local config_dir
    config_dir=$(dirname "$config_path")
    if [[ ! -d "$config_dir" ]]; then
        if ! mkdir -p "$config_dir" 2>/dev/null; then
            _needle_error "Failed to create config directory: $config_dir"
            return 1
        fi
    fi

    # Gather settings (interactive or defaults)
    local max_workers="$NEEDLE_DEFAULT_MAX_CONCURRENT"
    local default_agent="$NEEDLE_DEFAULT_AGENT"
    local telemetry="$NEEDLE_DEFAULT_TELEMETRY_ENABLED"
    local daily_limit="$NEEDLE_DEFAULT_DAILY_LIMIT"

    if [[ "$use_defaults" != "true" ]]; then
        _needle_section "Configuration Settings"

        max_workers=$(_needle_prompt_max_workers "$max_workers")
        default_agent=$(_needle_prompt_default_agent "$default_agent")
        telemetry=$(_needle_prompt_telemetry "$telemetry")
        daily_limit=$(_needle_prompt_daily_limit "$daily_limit")
    fi

    # Generate config
    local config_content
    config_content=$(_needle_generate_config_yaml \
        --max-workers "$max_workers" \
        --default-agent "$default_agent" \
        --telemetry "$telemetry" \
        --daily-limit "$daily_limit" \
    )

    # Write config file
    if ! echo "$config_content" > "$config_path" 2>/dev/null; then
        _needle_error "Failed to write config file: $config_path"
        return 1
    fi

    _needle_success "Created configuration: $config_path"

    # Validate the generated config
    if ! _needle_validate_generated_config "$config_path"; then
        _needle_warn "Config validation failed, but file was created"
        return 1
    fi

    return 0
}

# -----------------------------------------------------------------------------
# Config Validation
# -----------------------------------------------------------------------------

# Validate generated config file
# Usage: _needle_validate_generated_config <config_path>
# Returns: 0 if valid, 1 if invalid
_needle_validate_generated_config() {
    local config_path="$1"

    if [[ ! -f "$config_path" ]]; then
        _needle_error "Config file not found: $config_path"
        return 1
    fi

    if [[ ! -s "$config_path" ]]; then
        _needle_error "Config file is empty: $config_path"
        return 1
    fi

    _needle_debug "Validating config: $config_path"

    # Check required sections exist
    local required_sections=("needle:" "workers:" "telemetry:" "budget:")
    for section in "${required_sections[@]}"; do
        if ! grep -q "$section" "$config_path" 2>/dev/null; then
            _needle_error "Missing required section: $section"
            return 1
        fi
    done

    # Validate YAML syntax if yq is available
    if command -v yq &>/dev/null; then
        if ! yq eval '.' "$config_path" &>/dev/null; then
            _needle_error "Invalid YAML syntax in config file"
            return 1
        fi
    fi

    # Validate specific values
    local max_workers
    max_workers=$(grep -E "^\s+max_concurrent:" "$config_path" 2>/dev/null | head -1 | awk '{print $2}')

    if [[ -n "$max_workers" ]]; then
        if [[ ! "$max_workers" =~ ^[0-9]+$ ]] || [[ "$max_workers" -lt 1 ]]; then
            _needle_error "Invalid max_concurrent value: $max_workers"
            return 1
        fi
    fi

    local daily_limit
    daily_limit=$(grep -E "^\s+daily_limit_usd:" "$config_path" 2>/dev/null | head -1 | awk '{print $2}')

    if [[ -n "$daily_limit" ]]; then
        if [[ ! "$daily_limit" =~ ^[0-9]+\.?[0-9]*$ ]]; then
            _needle_error "Invalid daily_limit_usd value: $daily_limit"
            return 1
        fi
    fi

    local default_agent
    default_agent=$(grep -E "^\s+default_agent:" "$config_path" 2>/dev/null | head -1 | awk '{print $2}')

    if [[ -n "$default_agent" ]]; then
        case "$default_agent" in
            claude|opencode|codex|aider)
                ;;
            *)
                _needle_error "Invalid default_agent value: $default_agent"
                return 1
                ;;
        esac
    fi

    _needle_debug "Config validation passed"
    return 0
}

# -----------------------------------------------------------------------------
# Main Entry Point
# -----------------------------------------------------------------------------

# Create config during onboarding
# This is the main function called by needle init
# Usage: _needle_onboarding_create_config [options]
# Options:
#   --force               Overwrite existing config
#   --defaults            Use all default values (non-interactive)
#   --path PATH           Custom config path
# Returns: 0 on success, 1 on failure
_needle_onboarding_create_config() {
    _needle_section "Creating Configuration"

    if ! _needle_create_default_config "$@"; then
        _needle_error "Failed to create configuration"
        return 1
    fi

    return 0
}

# Quick config creation (non-interactive, all defaults)
# Usage: _needle_quick_create_config [--force] [--path PATH]
# Returns: 0 on success, 1 on failure
_needle_quick_create_config() {
    _needle_create_default_config --defaults "$@"
}
