#!/usr/bin/env bash
# NEEDLE CLI Configuration Management
# Load, validate, and manage configuration with defaults merging

# Config file location
NEEDLE_CONFIG_FILE="${NEEDLE_CONFIG_FILE:-$NEEDLE_HOME/$NEEDLE_CONFIG_NAME}"

# Cache variable (not exported, only for this session)
NEEDLE_CONFIG_CACHE=""

# Default configuration as JSON (used for merging)
_NEEDLE_CONFIG_DEFAULTS='{
  "billing": {
    "model": "pay_per_token",
    "daily_budget_usd": 10.0
  },
  "limits": {
    "global_max_concurrent": 20,
    "providers": {
      "anthropic": {
        "max_concurrent": 5,
        "requests_per_minute": 60
      }
    }
  },
  "runner": {
    "polling_interval": "2s",
    "idle_timeout": "300s"
  },
  "strands": {
    "pluck": "auto",
    "explore": "auto",
    "mend": "auto",
    "weave": "auto",
    "unravel": "auto",
    "pulse": "auto",
    "knot": "auto"
  },
  "mend": {
    "heartbeat_max_age": 3600,
    "max_log_files": 100,
    "min_interval": 60
  },
  "effort": {
    "budget": {
      "daily_limit_usd": 50.0,
      "warning_threshold": 0.8
    }
  },
  "hooks": {
    "timeout": "30s",
    "fail_action": "warn"
  },
  "mitosis": {
    "enabled": true,
    "skip_types": "bug,hotfix,incident",
    "skip_labels": "no-mitosis,atomic,single-task",
    "max_children": 5,
    "min_children": 2,
    "min_complexity": 100,
    "timeout": 60
  },
  "knot": {
    "rate_limit_interval": 3600
  },
  "weave": {
    "frequency": 3600,
    "max_beads_per_run": 5,
    "max_doc_files": 50
  },
  "unravel": {
    "min_wait_hours": 24,
    "max_alternatives": 3,
    "timeout": 120
  },
  "watchdog": {
    "interval": 30,
    "heartbeat_timeout": 120,
    "bead_timeout": 600,
    "recovery_action": "restart"
  },
  "pulse": {
    "frequency": "24h",
    "max_beads_per_run": 5,
    "seen_issues_retention_days": 30,
    "coverage_threshold": 70,
    "todo_age_days": 180,
    "max_todos_per_run": 10,
    "stale_threshold_days": 365,
    "max_deps_per_run": 10,
    "detectors": {
      "security": true,
      "dependencies": true,
      "docs": true,
      "doc_drift_enabled": true,
      "coverage": false,
      "todos": true
    }
  },
  "select": {
    "work_stealing_enabled": true,
    "work_stealing_timeout": 1800,
    "stealable_assignees": ["coder"],
    "check_worker_heartbeat": true,
    "unassigned_by_default": true,
    "proactive_stealing_enabled": true,
    "stealing_load_threshold": 2,
    "stealing_idle_threshold": 60,
    "stealing_priority_boost": 1,
    "steal_from_active_workers": false
  },
  "updates": {
    "check_on_startup": true,
    "check_interval": "24h",
    "auto_upgrade": false,
    "include_prereleases": false,
    "disabled": false
  },
  "file_locks": {
    "timeout": "30m",
    "stale_action": "warn"
  }
}'

# Default configuration as YAML (for creating new configs)
_NEEDLE_CONFIG_DEFAULTS_YAML='# NEEDLE Configuration
# See: https://github.com/user/needle#configuration

# Billing model configuration
# Controls how aggressively NEEDLE uses API budget
billing:
  # model: Billing model profile
  #   - pay_per_token: Conservative (default), minimize token usage
  #   - use_or_lose: Aggressive, use allocated budget
  #   - unlimited: Maximum throughput, no budget enforcement
  model: pay_per_token
  # daily_budget_usd: Daily budget in USD (used by use_or_lose model)
  daily_budget_usd: 10.0

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

# Maintenance strand configuration
mend:
  # heartbeat_max_age: Seconds before a stale heartbeat is considered dead (default: 1 hour)
  heartbeat_max_age: 3600
  # max_log_files: Maximum number of log files to keep (default: 100)
  max_log_files: 100
  # min_interval: Minimum seconds between mend runs (default: 60)
  min_interval: 60

effort:
  budget:
    daily_limit_usd: 50.0
    warning_threshold: 0.8

# Hook system for customizing NEEDLE behavior
# Hooks are user scripts that run at specific lifecycle events
# Exit codes: 0=success, 1=warning, 2=abort, 3=skip
hooks:
  # timeout: Maximum time a hook can run (e.g., 30s, 1m)
  timeout: 30s

  # fail_action: What to do when a hook fails (warn | abort | ignore)
  fail_action: warn

  # Hook paths (uncomment to enable):
  # pre_claim: ~/.needle/hooks/pre-claim.sh
  # post_claim: ~/.needle/hooks/post-claim.sh
  # pre_execute: ~/.needle/hooks/pre-execute.sh
  # post_execute: ~/.needle/hooks/post-execute.sh
  # pre_complete: ~/.needle/hooks/pre-complete.sh
  # post_complete: ~/.needle/hooks/post-complete.sh
  # on_failure: ~/.needle/hooks/on-failure.sh
  # on_quarantine: ~/.needle/hooks/on-quarantine.sh

# Mitosis configuration for automatic bead decomposition
# Mitosis splits complex beads into smaller, parallelizable subtasks
mitosis:
  # enabled: Enable/disable mitosis globally
  enabled: true

  # skip_types: Bead types that should not be split (comma-separated)
  skip_types: bug,hotfix,incident

  # skip_labels: Labels that prevent mitosis (comma-separated)
  skip_labels: no-mitosis,atomic,single-task

  # max_children: Maximum number of children per mitosis
  max_children: 5

  # min_children: Minimum children required to perform mitosis
  min_children: 2

  # min_complexity: Minimum description length (lines) to consider mitosis
  min_complexity: 100

  # timeout: Timeout in seconds for mitosis analysis
  timeout: 60

# Knot strand configuration (human alerts when stuck)
knot:
  # rate_limit_interval: Minimum seconds between stuck alerts per workspace (default: 1 hour)
  rate_limit_interval: 3600

# Weave strand configuration (documentation gap detection)
# This strand is opt-in only (disabled by default in strands.weave)
weave:
  # frequency: Minimum seconds between weave runs per workspace (default: 1 hour)
  frequency: 3600

  # max_beads_per_run: Maximum beads to create per weave analysis (default: 5)
  max_beads_per_run: 5

  # max_doc_files: Maximum documentation files to analyze per run (default: 50)
  max_doc_files: 50

# Unravel strand configuration (alternative approaches for blocked HUMAN beads)
# This strand is opt-in only (disabled by default in strands.unravel)
unravel:
  # min_wait_hours: Hours a HUMAN bead must wait before alternatives are proposed (default: 24)
  min_wait_hours: 24

  # max_alternatives: Maximum alternative beads to create per HUMAN bead (default: 3)
  max_alternatives: 3

  # timeout: Timeout in seconds for unravel analysis (default: 120)
  timeout: 120

# Watchdog configuration for automatic worker recovery
# The watchdog monitors heartbeats and recovers stuck workers
watchdog:
  # interval: Seconds between heartbeat checks (default: 30)
  interval: 30

  # heartbeat_timeout: Seconds without heartbeat before recovery (default: 120)
  heartbeat_timeout: 120

  # bead_timeout: Maximum seconds a bead can run before recovery (default: 600 = 10 minutes)
  bead_timeout: 600

  # recovery_action: What to do when a worker is stuck (restart | stop)
  # restart: Attempt to respawn the worker after recovery
  # stop: Just kill the worker without respawning
  recovery_action: restart

# Pulse strand configuration (codebase health monitoring)
# This strand is opt-in only (disabled by default in strands.pulse)
pulse:
  # frequency: Minimum time between pulse scans (default: 24h)
  # Supports: s (seconds), m (minutes), h (hours), d (days)
  frequency: 24h

  # max_beads_per_run: Maximum beads to create per scan (default: 5)
  max_beads_per_run: 5

  # seen_issues_retention_days: Days to remember seen issues for deduplication (default: 30)
  seen_issues_retention_days: 30

  # coverage_threshold: Minimum test coverage percentage (default: 70)
  coverage_threshold: 70

  # todo_age_days: Days before a TODO is considered stale (default: 180)
  todo_age_days: 180

  # max_todos_per_run: Maximum stale TODOs to report per scan (default: 10)
  max_todos_per_run: 10

  # stale_threshold_days: Days before a dependency is considered stale (default: 365)
  stale_threshold_days: 365

  # max_deps_per_run: Maximum stale dependencies to report per scan (default: 10)
  max_deps_per_run: 10

  # detectors: Enable/disable specific health detectors
  detectors:
    # security: Scan for security vulnerabilities
    security: true
    # dependencies: Check for outdated dependencies
    dependencies: true
    # docs: Check for documentation drift
    docs: true
    # doc_drift_enabled: Enable documentation reference drift detection
    doc_drift_enabled: true
    # coverage: Monitor test coverage trends (disabled by default)
    coverage: false
    # todos: Detect stale TODO/FIXME comments
    todos: true

# Work stealing configuration
# Allows idle workers to claim beads assigned to inactive assignees
select:
  # work_stealing_enabled: Enable/disable work stealing (default: true)
  work_stealing_enabled: true

  # work_stealing_timeout: Seconds before an assigned bead becomes stealable (default: 30 minutes)
  work_stealing_timeout: 1800

  # stealable_assignees: List of assignee names whose beads can be stolen (default: ["coder"])
  # "coder" is the human user - their beads become stealable when stale
  stealable_assignees:
    - coder

  # check_worker_heartbeat: Also check if assignee worker has no heartbeat (default: true)
  # If true, beads assigned to workers without heartbeats become stealable immediately
  check_worker_heartbeat: true

  # unassigned_by_default: Leave new beads unassigned for workers to claim (default: true)
  # When true, beads created by NEEDLE are immediately released after creation,
  # allowing workers to claim them without waiting for work_stealing_timeout.
  # This prevents worker starvation when all beads are auto-assigned to the creator.
  unassigned_by_default: true

  # === Advanced Work Stealing (Idle Worker Features) ===

  # proactive_stealing_enabled: Enable proactive work stealing by idle workers (default: true)
  # When enabled, idle workers will actively try to steal work from busy workers
  proactive_stealing_enabled: true

  # stealing_load_threshold: Min claimed beads before worker is "overloaded" (default: 2)
  # Workers with this many or more claimed beads become targets for work stealing
  stealing_load_threshold: 2

  # stealing_idle_threshold: Seconds of idle time before worker can steal (default: 60)
  # Workers must be idle for this long before they start stealing from others
  stealing_idle_threshold: 60

  # stealing_priority_boost: Priority weight multiplier for stolen beads (default: 1)
  # Higher values make stolen beads more likely to be selected
  stealing_priority_boost: 1

  # steal_from_active_workers: Allow stealing from workers with heartbeats (default: false)
  # WARNING: This can cause duplicate work if workers are actually processing
  # Only enable if workers are known to be single-threaded and reliable
  steal_from_active_workers: false

# Self-update configuration
# Controls how NEEDLE checks for and installs updates
updates:
  # check_on_startup: Check for updates when needle starts (default: true)
  # Displays non-blocking notification if update is available
  check_on_startup: true

  # check_interval: How often to check for updates (default: 24h)
  # Supports: s (seconds), m (minutes), h (hours), d (days)
  check_interval: 24h

  # auto_upgrade: Automatically install updates without prompting (default: false)
  auto_upgrade: false

  # include_prereleases: Include pre-release versions in update checks (default: false)
  include_prereleases: false

  # disabled: Completely disable update checks (for air-gapped environments)
  disabled: false

# File lock configuration for collision management
# Prevents multiple workers from editing the same file simultaneously
file_locks:
  # timeout: Maximum time a lock can be held before considered stale (default: 30m)
  # Supports: s (seconds), m (minutes), h (hours)
  timeout: 30m

  # stale_action: What to do when a stale lock is detected (default: warn)
  # Options: warn (log warning), release (remove lock), ignore (do nothing)
  stale_action: warn
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

# Convert YAML to JSON using Python (fallback when yq not available)
# Usage: _needle_yaml_to_json <file>
_needle_yaml_to_json() {
    local config_file="$1"

    if [[ ! -f "$config_file" ]]; then
        echo "{}"
        return 1
    fi

    python3 -c "
import yaml
import json
import sys

try:
    with open('$config_file', 'r') as f:
        data = yaml.safe_load(f)

    if data is None:
        data = {}

    print(json.dumps(data))
except Exception as e:
    print(f'Error: {e}', file=sys.stderr)
    sys.exit(1)
" 2>/dev/null
}

# Simple YAML merge fallback (without yq)
# Uses Python to properly merge YAML with defaults
_needle_simple_yaml_merge() {
    local config_file="$1"

    # Check if Python PyYAML is available
    if python3 -c "import yaml" 2>/dev/null; then
        # Use Python for proper YAML parsing and merging
        # Pass defaults via stdin to avoid shell escaping issues
        python3 -c "
import yaml
import json
import sys

# Read defaults from environment
defaults_json = '''$_NEEDLE_CONFIG_DEFAULTS'''

try:
    defaults = json.loads(defaults_json)
except:
    defaults = {}

def deep_merge(base, override):
    '''Recursively merge override into base'''
    result = dict(base)
    for key, value in override.items():
        if key in result and isinstance(result[key], dict) and isinstance(value, dict):
            result[key] = deep_merge(result[key], value)
        else:
            result[key] = value
    return result

try:
    with open('$config_file', 'r') as f:
        user_config = yaml.safe_load(f)

    if user_config is None:
        user_config = {}

    merged = deep_merge(defaults, user_config)
    print(json.dumps(merged))
except Exception as e:
    # On error, just return defaults
    print(json.dumps(defaults))
" 2>/dev/null
        return $?
    fi

    # Fallback to simple string-based merge (limited)
    local result="$_NEEDLE_CONFIG_DEFAULTS"

    # Read simple top-level settings and update
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
            case "$key" in
                global_max_concurrent|timeout|max_workers)
                    if [[ "$value" =~ ^[0-9]+$ ]]; then
                        result=$(echo "$result" | sed "s/\"$key\":[^,}]*/\"$key\": $value/")
                    fi
                    ;;
                polling_interval|idle_timeout)
                    result=$(echo "$result" | sed "s/\"$key\":[^,}]*/\"$key\": \"$value\"/")
                    ;;
            esac
        fi
    done < "$config_file"

    echo "$result"
}

# Convert dot-notation path to jq bracket notation
# Handles keys with hyphens by converting to bracket notation
# Example: "limits.models.claude-anthropic-opus.max_concurrent"
#   -> ".limits.models[\"claude-anthropic-opus\"].max_concurrent"
_needle_path_to_jq() {
    local path="$1"
    local jq_path=""
    local IFS='.'

    read -ra parts <<< "$path"
    for part in "${parts[@]}"; do
        if [[ -z "$part" ]]; then
            continue
        fi
        # Check if part contains hyphen (or other special chars)
        if [[ "$part" == *-* ]] || [[ "$part" == *" "* ]]; then
            jq_path="${jq_path}[\"$part\"]"
        else
            jq_path="${jq_path}.$part"
        fi
    done

    echo "$jq_path"
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
        # Fallback: simple JSON parsing with jq
        if command -v jq &>/dev/null; then
            local jq_path
            jq_path=$(_needle_path_to_jq "$key")
            value=$(load_config | jq -r "$jq_path" 2>/dev/null)
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

# Billing model configuration
# Controls how aggressively NEEDLE uses API budget
billing:
  # model: Billing model profile
  #   - pay_per_token: Conservative (default), minimize token usage
  #   - use_or_lose: Aggressive, use allocated budget
  #   - unlimited: Maximum throughput, no budget enforcement
  model: pay_per_token
  # daily_budget_usd: Daily budget in USD (used by use_or_lose model)
  daily_budget_usd: 10.0

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

# Maintenance strand configuration
mend:
  # heartbeat_max_age: Seconds before a stale heartbeat is considered dead (default: 1 hour)
  heartbeat_max_age: 3600
  # max_log_files: Maximum number of log files to keep (default: 100)
  max_log_files: 100
  # min_interval: Minimum seconds between mend runs (default: 60)
  min_interval: 60

effort:
  budget:
    daily_limit_usd: 50.0
    warning_threshold: 0.8

# Hook system for customizing NEEDLE behavior
# Hooks are user scripts that run at specific lifecycle events
# Exit codes: 0=success, 1=warning, 2=abort, 3=skip
hooks:
  # timeout: Maximum time a hook can run (e.g., 30s, 1m)
  timeout: 30s

  # fail_action: What to do when a hook fails (warn | abort | ignore)
  fail_action: warn

  # Hook paths (uncomment to enable):
  # pre_claim: ~/.needle/hooks/pre-claim.sh
  # post_claim: ~/.needle/hooks/post-claim.sh
  # pre_execute: ~/.needle/hooks/pre-execute.sh
  # post_execute: ~/.needle/hooks/post-execute.sh
  # pre_complete: ~/.needle/hooks/pre-complete.sh
  # post_complete: ~/.needle/hooks/post-complete.sh
  # on_failure: ~/.needle/hooks/on-failure.sh
  # on_quarantine: ~/.needle/hooks/on-quarantine.sh

# Mitosis configuration for automatic bead decomposition
# Mitosis splits complex beads into smaller, parallelizable subtasks
mitosis:
  # enabled: Enable/disable mitosis globally
  enabled: true

  # skip_types: Bead types that should not be split (comma-separated)
  skip_types: bug,hotfix,incident

  # skip_labels: Labels that prevent mitosis (comma-separated)
  skip_labels: no-mitosis,atomic,single-task

  # max_children: Maximum number of children per mitosis
  max_children: 5

  # min_children: Minimum children required to perform mitosis
  min_children: 2

  # min_complexity: Minimum description length (lines) to consider mitosis
  min_complexity: 100

  # timeout: Timeout in seconds for mitosis analysis
  timeout: 60

# Knot strand configuration (human alerts when stuck)
knot:
  # rate_limit_interval: Minimum seconds between stuck alerts per workspace (default: 1 hour)
  rate_limit_interval: 3600

# Unravel strand configuration (alternative approaches for blocked HUMAN beads)
# This strand is opt-in only (disabled by default in strands.unravel)
unravel:
  # min_wait_hours: Hours a HUMAN bead must wait before alternatives are proposed (default: 24)
  min_wait_hours: 24

  # max_alternatives: Maximum alternative beads to create per HUMAN bead (default: 3)
  max_alternatives: 3

  # timeout: Timeout in seconds for unravel analysis (default: 120)
  timeout: 120

# Work stealing configuration for multi-worker environments
# Allows workers to claim beads assigned to inactive humans/workers
select:
  # work_stealing_enabled: Enable/disable work stealing (default: true)
  work_stealing_enabled: true

  # work_stealing_timeout: Seconds before an assigned bead becomes stealable (default: 30 minutes)
  work_stealing_timeout: 1800

  # stealable_assignees: Users whose assignments can be stolen (default: ["coder"])
  stealable_assignees:
    - coder

  # check_worker_heartbeat: Verify if assigned worker is still alive (default: true)
  check_worker_heartbeat: true

  # unassigned_by_default: Leave new beads unassigned for workers to claim (default: true)
  # When true, beads created by NEEDLE are immediately released after creation,
  # allowing workers to claim them without waiting for work_stealing_timeout.
  unassigned_by_default: true

  # === Advanced Work Stealing (Idle Worker Features) ===

  # proactive_stealing_enabled: Enable proactive work stealing by idle workers (default: true)
  proactive_stealing_enabled: true

  # stealing_load_threshold: Min claimed beads before worker is "overloaded" (default: 2)
  stealing_load_threshold: 2

  # stealing_idle_threshold: Seconds of idle time before worker can steal (default: 60)
  stealing_idle_threshold: 60

  # stealing_priority_boost: Priority weight multiplier for stolen beads (default: 1)
  stealing_priority_boost: 1

  # steal_from_active_workers: Allow stealing from workers with heartbeats (default: false)
  steal_from_active_workers: false

# Self-update configuration
# Controls how NEEDLE checks for and installs updates
updates:
  # check_on_startup: Check for updates when needle starts (default: true)
  check_on_startup: true

  # check_interval: How often to check for updates (default: 24h)
  # Supports: s (seconds), m (minutes), h (hours), d (days)
  check_interval: 24h

  # auto_upgrade: Automatically install updates without prompting (default: false)
  auto_upgrade: false

  # include_prereleases: Include pre-release versions in update checks (default: false)
  include_prereleases: false

  # disabled: Completely disable update checks (for air-gapped environments)
  disabled: false

# File lock configuration for collision management
# Prevents multiple workers from editing the same file simultaneously
file_locks:
  # timeout: Maximum time a lock can be held before considered stale (default: 30m)
  # Supports: s (seconds), m (minutes), h (hours)
  timeout: 30m

  # stale_action: What to do when a stale lock is detected (default: warn)
  # Options: warn (log warning), release (remove lock), ignore (do nothing)
  stale_action: warn
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

    # Validate billing model
    local billing_model
    billing_model=$(get_config "billing.model" "pay_per_token")

    case "$billing_model" in
        pay_per_token|use_or_lose|unlimited)
            ;;
        *)
            _needle_error "Invalid billing.model: must be pay_per_token, use_or_lose, or unlimited"
            return 1
            ;;
    esac

    local billing_budget
    billing_budget=$(get_config "billing.daily_budget_usd" "10.0")

    if [[ ! "$billing_budget" =~ ^[0-9]+\.?[0-9]*$ ]] || [[ "$(echo "$billing_budget < 0" | bc 2>/dev/null || echo 0)" -eq 1 ]]; then
        _needle_error "Invalid billing.daily_budget_usd: must be positive number"
        return 1
    fi

    # Validate mitosis configuration
    local mitosis_max_children
    mitosis_max_children=$(get_config "mitosis.max_children" "5")

    if [[ ! "$mitosis_max_children" =~ ^[0-9]+$ ]] || [[ "$mitosis_max_children" -lt 1 ]]; then
        _needle_error "Invalid mitosis.max_children: must be positive integer > 0"
        return 1
    fi

    local mitosis_min_complexity
    mitosis_min_complexity=$(get_config "mitosis.min_complexity" "100")

    if [[ ! "$mitosis_min_complexity" =~ ^[0-9]+$ ]] || [[ "$mitosis_min_complexity" -lt 0 ]]; then
        _needle_error "Invalid mitosis.min_complexity: must be non-negative integer"
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
