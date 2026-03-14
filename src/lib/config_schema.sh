#!/usr/bin/env bash
# NEEDLE Config Schema Validation
# Defines the schema for config.yaml and .needle.yaml, and validates
# configs on load with clear error messages.
#
# Provides:
#   validate_config_schema <config_file>  - Full schema validation
#   check_deprecated_keys <config_file>   - Detect deprecated config keys
#   validate_strand_config <config_json>  - Check strand flags vs implemented strands

# Guard against double-sourcing
[[ -n "${_NEEDLE_CONFIG_SCHEMA_LOADED:-}" ]] && return 0
_NEEDLE_CONFIG_SCHEMA_LOADED=1

# ============================================================================
# Strand Config
# ============================================================================
# Strands are configured as an ordered YAML list of script paths.
# No hardcoded strand names — the config list is the source of truth.

# ============================================================================
# Deprecated Keys
# ============================================================================
# Format: deprecated_dotpath="message describing replacement"
# Keys are dot-notation paths matching the YAML structure.
#
# When a deprecated key is found we warn (never error), so configs keep working
# while guiding users toward the new location.
declare -A NEEDLE_DEPRECATED_KEYS
NEEDLE_DEPRECATED_KEYS=(
    ["effort.budget.daily_limit_usd"]="Deprecated: use 'billing.daily_budget_usd' instead"
    ["effort.budget.warning_threshold"]="Deprecated: 'effort.budget.warning_threshold' is no longer used"
    ["strands.pluck"]="Deprecated: strands is now an ordered list of script paths, not a map of name:enabled"
    ["strands.explore"]="Deprecated: strands is now an ordered list of script paths, not a map of name:enabled"
    ["strands.mend"]="Deprecated: strands is now an ordered list of script paths, not a map of name:enabled"
    ["strands.weave"]="Deprecated: strands is now an ordered list of script paths, not a map of name:enabled"
    ["strands.unravel"]="Deprecated: strands is now an ordered list of script paths, not a map of name:enabled"
    ["strands.pulse"]="Deprecated: strands is now an ordered list of script paths, not a map of name:enabled"
    ["strands.knot"]="Deprecated: strands is now an ordered list of script paths, not a map of name:enabled"
    ["strands.weave.frequency"]="Deprecated flat key: use 'weave.frequency' instead"
    ["strands.weave.max_doc_files"]="Deprecated flat key: use 'weave.max_doc_files' instead"
    ["strands.weave.max_beads_per_run"]="Deprecated flat key: use 'weave.max_beads_per_run' instead"
    ["runner.max_workers"]="Deprecated: use 'limits.global_max_concurrent' instead"
    ["global_max_concurrent"]="Deprecated top-level key: use 'limits.global_max_concurrent' instead"
)

# ============================================================================
# Schema Field Definitions
# ============================================================================
# Each entry is: "type|required|min|max|allowed_values"
#   type:           string | integer | float | boolean | duration | enum
#   required:       true | false
#   min:            numeric minimum (for integer/float), empty = no limit
#   max:            numeric maximum (for integer/float), empty = no limit
#   allowed_values: pipe-separated enum values (for enum type), empty = any
#
declare -A NEEDLE_CONFIG_SCHEMA
NEEDLE_CONFIG_SCHEMA=(
    # billing
    ["billing.model"]="enum|false|||pay_per_token|use_or_lose|unlimited"
    ["billing.daily_budget_usd"]="float|false|0||"

    # limits
    ["limits.global_max_concurrent"]="integer|false|1||"
    ["limits.providers.anthropic.max_concurrent"]="integer|false|1||"
    ["limits.providers.anthropic.requests_per_minute"]="integer|false|1||"

    # runner
    ["runner.polling_interval"]="duration|false|||"
    ["runner.idle_timeout"]="duration|false|||"

    # mend
    ["mend.heartbeat_max_age"]="integer|false|1||"
    ["mend.max_log_files"]="integer|false|1||"
    ["mend.min_interval"]="integer|false|0||"

    # hooks
    ["hooks.timeout"]="duration|false|||"
    ["hooks.fail_action"]="enum|false|||warn|abort|ignore"

    # mitosis
    ["mitosis.enabled"]="boolean|false|||"
    ["mitosis.max_children"]="integer|false|1||"
    ["mitosis.min_children"]="integer|false|1||"
    ["mitosis.min_complexity"]="integer|false|0||"
    ["mitosis.timeout"]="integer|false|1||"

    # knot
    ["knot.rate_limit_interval"]="integer|false|0||"

    # weave
    ["weave.frequency"]="integer|false|0||"
    ["weave.max_beads_per_run"]="integer|false|1||"
    ["weave.max_doc_files"]="integer|false|1||"

    # unravel
    ["unravel.min_wait_hours"]="integer|false|0||"
    ["unravel.max_alternatives"]="integer|false|1||"
    ["unravel.timeout"]="integer|false|1||"

    # watchdog
    ["watchdog.interval"]="integer|false|1||"
    ["watchdog.heartbeat_timeout"]="integer|false|1||"
    ["watchdog.bead_timeout"]="integer|false|1||"
    ["watchdog.recovery_action"]="enum|false|||restart|stop"
    ["watchdog.startup_grace"]="integer|false|0||"

    # pulse
    ["pulse.frequency"]="duration|false|||"
    ["pulse.max_beads_per_run"]="integer|false|1||"
    ["pulse.seen_issues_retention_days"]="integer|false|1||"
    ["pulse.coverage_threshold"]="integer|false|0|100|"
    ["pulse.todo_age_days"]="integer|false|1||"
    ["pulse.max_todos_per_run"]="integer|false|1||"
    ["pulse.stale_threshold_days"]="integer|false|1||"
    ["pulse.max_deps_per_run"]="integer|false|1||"
    ["pulse.detectors.security"]="boolean|false|||"
    ["pulse.detectors.dependencies"]="boolean|false|||"
    ["pulse.detectors.docs"]="boolean|false|||"
    ["pulse.detectors.doc_drift_enabled"]="boolean|false|||"
    ["pulse.detectors.coverage"]="boolean|false|||"
    ["pulse.detectors.todos"]="boolean|false|||"

    # select
    ["select.work_stealing_enabled"]="boolean|false|||"
    ["select.work_stealing_timeout"]="integer|false|0||"
    ["select.check_worker_heartbeat"]="boolean|false|||"
    ["select.unassigned_by_default"]="boolean|false|||"
    ["select.proactive_stealing_enabled"]="boolean|false|||"
    ["select.stealing_load_threshold"]="integer|false|1||"
    ["select.stealing_idle_threshold"]="integer|false|0||"
    ["select.stealing_priority_boost"]="integer|false|0||"
    ["select.steal_from_active_workers"]="boolean|false|||"

    # scaling
    ["scaling.spawn_threshold"]="integer|false|1||"
    ["scaling.max_workers_per_agent"]="integer|false|1||"
    ["scaling.cooldown_seconds"]="integer|false|0||"

    # updates
    ["updates.check_on_startup"]="boolean|false|||"
    ["updates.check_interval"]="duration|false|||"
    ["updates.auto_upgrade"]="boolean|false|||"
    ["updates.include_prereleases"]="boolean|false|||"
    ["updates.disabled"]="boolean|false|||"

    # file_locks
    ["file_locks.timeout"]="duration|false|||"
    ["file_locks.stale_action"]="enum|false|||warn|release|ignore"
    ["file_locks.ld_preload"]="boolean|false|||"
    ["file_locks.ld_preload_lib"]="string|false|||"
    ["file_locks.strategy"]="enum|false|||pessimistic|optimistic"
    ["file_locks.merge.enabled"]="boolean|false|||"
    ["file_locks.merge.tool"]="enum|false|||git-merge-file|diff3|custom"
    ["file_locks.merge.on_conflict"]="enum|false|||block|keep_ours|keep_theirs"
    ["file_locks.lease.duration"]="duration|false|||"
    ["file_locks.lease.renewal_interval"]="duration|false|||"
    ["file_locks.lease.grace_period"]="duration|false|||"

    # fabric
    ["fabric.enabled"]="boolean|false|||"
    ["fabric.endpoint"]="string|false|||"
    ["fabric.timeout"]="integer|false|0||"
    ["fabric.batching"]="boolean|false|||"

    # debug
    ["debug.auto_bead_on_error"]="boolean|false|||"
    ["debug.auto_bead_workspace"]="string|false|||"
    ["debug.auto_bead_rate_limit"]="integer|false|0||"
)

# ============================================================================
# Internal Helpers
# ============================================================================

# Emit a schema validation error message
_schema_error() {
    echo "ERROR: Config validation: $*" >&2
}

# Emit a schema validation warning message (non-fatal)
_schema_warn() {
    echo "WARN: Config validation: $*" >&2
}

# Check if a value is a valid boolean
_schema_is_boolean() {
    local v="$1"
    case "$v" in
        true|True|TRUE|false|False|FALSE|yes|no|1|0) return 0 ;;
        *) return 1 ;;
    esac
}

# Check if a value is a valid integer (optionally with sign)
_schema_is_integer() {
    [[ "$1" =~ ^-?[0-9]+$ ]]
}

# Check if a value is a valid float (e.g. 10.5, 10, .5)
_schema_is_float() {
    [[ "$1" =~ ^-?[0-9]*\.?[0-9]+$ ]]
}

# Check if a value looks like a valid duration (e.g. 30s, 5m, 2h, 1d)
_schema_is_duration() {
    [[ "$1" =~ ^[0-9]+[smhd]?$ ]]
}

# Check that a numeric value is within [min, max]
# Usage: _schema_check_range <value> <min> <max>
# Pass empty string for no limit on either end.
_schema_check_range() {
    local val="$1" min="$2" max="$3"
    if [[ -n "$min" ]] && _schema_is_float "$min" && _schema_is_float "$val"; then
        if python3 -c "exit(0 if float('$val') >= float('$min') else 1)" 2>/dev/null; then
            :
        else
            return 1
        fi
    fi
    if [[ -n "$max" ]] && _schema_is_float "$max" && _schema_is_float "$val"; then
        if python3 -c "exit(0 if float('$val') <= float('$max') else 1)" 2>/dev/null; then
            :
        else
            return 1
        fi
    fi
    return 0
}

# Extract a YAML/JSON value from a config file using yq or python
# Returns the raw string value, or empty string if not present.
# Usage: _schema_get_value <config_file> <dotpath>
_schema_get_value() {
    local config_file="$1"
    local dotpath="$2"

    if command -v yq &>/dev/null; then
        yq ".$dotpath" "$config_file" 2>/dev/null | grep -v '^null$'
    elif python3 -c "import yaml" 2>/dev/null; then
        python3 - "$config_file" "$dotpath" <<'PYEOF' 2>/dev/null
import yaml, sys

def get_nested(d, path):
    parts = path.split('.')
    for p in parts:
        if not isinstance(d, dict):
            return None
        d = d.get(p)
    return d

with open(sys.argv[1]) as f:
    data = yaml.safe_load(f) or {}

val = get_nested(data, sys.argv[2])
if val is None:
    sys.exit(0)
print(val)
PYEOF
    fi
}

# Get all top-level keys from a YAML config file
_schema_get_top_level_keys() {
    local config_file="$1"
    if command -v yq &>/dev/null; then
        yq 'keys | .[]' "$config_file" 2>/dev/null
    elif python3 -c "import yaml" 2>/dev/null; then
        python3 - "$config_file" <<'PYEOF' 2>/dev/null
import yaml, sys
with open(sys.argv[1]) as f:
    data = yaml.safe_load(f) or {}
for k in data.keys():
    print(k)
PYEOF
    fi
}

# Get all keys from the strands section
_schema_get_strand_keys() {
    local config_file="$1"
    if command -v yq &>/dev/null; then
        yq '.strands | keys | .[]' "$config_file" 2>/dev/null | grep -v '^null$'
    elif python3 -c "import yaml" 2>/dev/null; then
        python3 - "$config_file" <<'PYEOF' 2>/dev/null
import yaml, sys
with open(sys.argv[1]) as f:
    data = yaml.safe_load(f) or {}
strands = data.get('strands', {})
if isinstance(strands, dict):
    for k in strands.keys():
        print(k)
PYEOF
    fi
}

# ============================================================================
# check_deprecated_keys
# ============================================================================
# Scan config file for deprecated key paths and emit warnings.
# Never returns non-zero (deprecated keys are warnings, not errors).
#
# Usage: check_deprecated_keys <config_file>
check_deprecated_keys() {
    local config_file="${1:-$NEEDLE_CONFIG_FILE}"
    local found_any=0

    if [[ ! -f "$config_file" ]]; then
        return 0
    fi

    for deprecated_path in "${!NEEDLE_DEPRECATED_KEYS[@]}"; do
        local val
        val=$(_schema_get_value "$config_file" "$deprecated_path")
        if [[ -n "$val" ]]; then
            local msg="${NEEDLE_DEPRECATED_KEYS[$deprecated_path]}"
            _schema_warn "$msg (found at: $deprecated_path)"
            found_any=1
        fi
    done

    return 0
}

# ============================================================================
# validate_strand_config
# ============================================================================
# Validate that strands: is a YAML list of script path strings.
#
# Usage: validate_strand_config <config_file>
# Returns: 0 if valid, 1 if invalid
validate_strand_config() {
    local config_file="${1:-$NEEDLE_CONFIG_FILE}"
    local errors=0

    if [[ ! -f "$config_file" ]]; then
        return 0
    fi

    # Check if strands key exists
    local strands_raw
    strands_raw=$(_schema_get_value "$config_file" "strands")

    if [[ -z "$strands_raw" ]]; then
        # No strands section — defaults will be used
        return 0
    fi

    # Validate strands is a list (not a map or scalar)
    if command -v yq &>/dev/null; then
        local strands_type
        strands_type=$(yq '.strands | tag' "$config_file" 2>/dev/null)
        if [[ "$strands_type" != "!!seq" ]]; then
            _schema_error "strands: expected a list of script paths, got $strands_type"
            return 1
        fi

        # Validate each entry is a non-empty string
        local entries
        entries=$(yq '.strands[]' "$config_file" 2>/dev/null)
        local idx=0
        while IFS= read -r entry; do
            [[ -z "$entry" ]] && continue
            if [[ "$entry" == "null" ]]; then
                _schema_error "strands[$idx]: null entry not allowed"
                ((errors++))
            fi
            ((idx++))
        done <<< "$entries"
    elif python3 -c "import yaml" 2>/dev/null; then
        local result
        result=$(python3 - "$config_file" <<'PYEOF' 2>&1
import yaml, sys
with open(sys.argv[1]) as f:
    data = yaml.safe_load(f) or {}
strands = data.get('strands')
if strands is None:
    sys.exit(0)
if not isinstance(strands, list):
    print(f"ERROR: strands: expected a list, got {type(strands).__name__}", file=sys.stderr)
    sys.exit(1)
for i, entry in enumerate(strands):
    if not isinstance(entry, str) or not entry.strip():
        print(f"ERROR: strands[{i}]: expected non-empty string, got {type(entry).__name__}: {entry!r}", file=sys.stderr)
        sys.exit(1)
sys.exit(0)
PYEOF
)
        if [[ $? -ne 0 ]]; then
            ((errors++))
        fi
    fi

    [[ "$errors" -eq 0 ]]
}

# ============================================================================
# validate_config_schema
# ============================================================================
# Full schema validation of a NEEDLE config file.
# Validates field types, ranges, enum values, and strand config.
# Also runs deprecated key detection (warnings only).
#
# Usage: validate_config_schema [config_file]
# Returns: 0 if valid, 1 if there are errors
validate_config_schema() {
    local config_file="${1:-$NEEDLE_CONFIG_FILE}"
    local errors=0

    if [[ ! -f "$config_file" ]]; then
        # Non-existent config is fine - defaults will be used
        return 0
    fi

    if [[ ! -s "$config_file" ]]; then
        # Empty config is fine - defaults will be used
        return 0
    fi

    # Check YAML syntax first
    if command -v yq &>/dev/null; then
        if ! yq eval '.' "$config_file" &>/dev/null; then
            _schema_error "Invalid YAML syntax in $config_file"
            return 1
        fi
    elif python3 -c "import yaml" 2>/dev/null; then
        if ! python3 -c "import yaml; yaml.safe_load(open('$config_file'))" 2>/dev/null; then
            _schema_error "Invalid YAML syntax in $config_file"
            return 1
        fi
    fi

    # Check deprecated keys (warnings only, no errors)
    check_deprecated_keys "$config_file"

    # Validate strand config (unknown strands + invalid values)
    if ! validate_strand_config "$config_file"; then
        ((errors++))
    fi

    # Validate each field defined in the schema
    for field_path in "${!NEEDLE_CONFIG_SCHEMA[@]}"; do
        local spec="${NEEDLE_CONFIG_SCHEMA[$field_path]}"
        local field_type required min_val max_val allowed_vals_raw

        # Parse spec: type|required|min|max|allowed_values (pipe-separated enum vals)
        IFS='|' read -r field_type required min_val max_val allowed_vals_raw <<< "$spec"

        # Get the value from the config file
        local raw_val
        raw_val=$(_schema_get_value "$config_file" "$field_path")

        # If the value is not set, skip (no required fields in our schema currently)
        if [[ -z "$raw_val" ]]; then
            if [[ "$required" == "true" ]]; then
                _schema_error "$field_path: required field is missing"
                ((errors++))
            fi
            continue
        fi

        # Validate by type
        local type_ok=1
        case "$field_type" in
            boolean)
                if ! _schema_is_boolean "$raw_val"; then
                    _schema_error "$field_path: expected boolean (true/false), got '$raw_val'"
                    type_ok=0
                    ((errors++))
                fi
                ;;
            integer)
                if ! _schema_is_integer "$raw_val"; then
                    _schema_error "$field_path: expected integer, got '$raw_val'"
                    type_ok=0
                    ((errors++))
                fi
                ;;
            float)
                if ! _schema_is_float "$raw_val"; then
                    _schema_error "$field_path: expected number, got '$raw_val'"
                    type_ok=0
                    ((errors++))
                fi
                ;;
            duration)
                if ! _schema_is_duration "$raw_val"; then
                    _schema_error "$field_path: expected duration (e.g. 30s, 5m, 2h), got '$raw_val'"
                    type_ok=0
                    ((errors++))
                fi
                ;;
            enum)
                # allowed_vals_raw contains the remaining pipe-separated values
                # (the split above already consumed 4 fields, rest is enum vals)
                local found_in_enum=0
                # Rebuild allowed values from remaining fields
                local all_fields_str="$spec"
                # Split on | and take from index 4 onward
                local -a all_fields
                IFS='|' read -ra all_fields <<< "$all_fields_str"
                for ((i=4; i<${#all_fields[@]}; i++)); do
                    local allowed="${all_fields[$i]}"
                    [[ -z "$allowed" ]] && continue
                    if [[ "$raw_val" == "$allowed" ]]; then
                        found_in_enum=1
                        break
                    fi
                done
                if [[ "$found_in_enum" -eq 0 ]]; then
                    # Build display list
                    local enum_vals=()
                    for ((i=4; i<${#all_fields[@]}; i++)); do
                        [[ -n "${all_fields[$i]}" ]] && enum_vals+=("${all_fields[$i]}")
                    done
                    _schema_error "$field_path: invalid value '$raw_val'. Allowed: ${enum_vals[*]}"
                    type_ok=0
                    ((errors++))
                fi
                ;;
            string)
                # Any non-empty string is valid
                ;;
        esac

        # Range check (only for integer/float and if type was ok)
        if [[ "$type_ok" -eq 1 ]] && { [[ "$field_type" == "integer" ]] || [[ "$field_type" == "float" ]]; }; then
            if [[ -n "$min_val" ]] || [[ -n "$max_val" ]]; then
                if ! _schema_check_range "$raw_val" "$min_val" "$max_val"; then
                    local range_desc=""
                    [[ -n "$min_val" ]] && range_desc+=" >= $min_val"
                    [[ -n "$max_val" ]] && range_desc+=" <= $max_val"
                    _schema_error "$field_path: value '$raw_val' out of range (must be$range_desc)"
                    ((errors++))
                fi
            fi
        fi
    done

    if [[ "$errors" -gt 0 ]]; then
        _schema_error "Config validation failed with $errors error(s): $config_file"
        return 1
    fi

    return 0
}

# ============================================================================
# validate_config_on_load
# ============================================================================
# Combined entry point: run schema validation when config is loaded.
# Intended to be called from load_config() or validate_config().
# Errors are printed but do not prevent the config from loading
# (the system falls back to defaults for invalid values).
#
# Usage: validate_config_on_load [config_file]
# Returns: 0 always (so config loading is not blocked by schema errors)
validate_config_on_load() {
    local config_file="${1:-$NEEDLE_CONFIG_FILE}"

    # Run full schema validation - collect errors but don't abort
    if ! validate_config_schema "$config_file" 2>&1; then
        # Already printed errors via _schema_error
        return 1
    fi

    return 0
}

# ============================================================================
# validate_preferred_agents
# ============================================================================
# Validate the preferred_agents field in a workspace config.
# preferred_agents should be an array of non-empty strings (agent names).
#
# Usage: validate_preferred_agents <config_file>
# Returns: 0 if valid or not present, 1 if invalid
validate_preferred_agents() {
    local config_file="${1:-}"

    if [[ -z "$config_file" ]] || [[ ! -f "$config_file" ]]; then
        return 0
    fi

    # Check if preferred_agents exists in the config using Python (most reliable)
    if python3 -c "import yaml" 2>/dev/null; then
        local validation_result
        validation_result=$(python3 - "$config_file" <<'PYEOF' 2>&1
import yaml, sys

def validate_preferred_agents(filepath):
    errors = []
    try:
        with open(filepath) as f:
            data = yaml.safe_load(f) or {}

        pa = data.get('preferred_agents')

        # Not present is fine
        if pa is None:
            return []

        # Must be a list
        if not isinstance(pa, list):
            errors.append(f"preferred_agents: expected array, got {type(pa).__name__}")
            return errors

        # Validate each item
        import re
        agent_pattern = re.compile(r'^[a-zA-Z0-9_-]+$')
        for i, item in enumerate(pa):
            if not isinstance(item, str):
                errors.append(f"preferred_agents[{i}]: expected string, got {type(item).__name__}")
            elif not item:
                errors.append(f"preferred_agents[{i}]: empty string not allowed")
            elif not agent_pattern.match(item):
                errors.append(f"preferred_agents[{i}]: invalid agent name '{item}' (must be alphanumeric with hyphens/underscores)")

        return errors
    except Exception as e:
        return [f"YAML parsing error: {e}"]

errors = validate_preferred_agents(sys.argv[1])
if errors:
    for e in errors:
        print(f"ERROR: {e}", file=sys.stderr)
    sys.exit(1)
sys.exit(0)
PYEOF
)
        return $?
    fi

    # Fallback: check with yq if Python not available
    if command -v yq &>/dev/null; then
        local pa_type
        pa_type=$(yq '.preferred_agents | tag' "$config_file" 2>/dev/null)

        # Not present is fine
        if [[ -z "$pa_type" ]] || [[ "$pa_type" == "!!null" ]]; then
            return 0
        fi

        # Must be an array (!!seq in YAML)
        if [[ "$pa_type" != "!!seq" ]]; then
            _schema_error "preferred_agents: expected array, got $pa_type"
            return 1
        fi

        # Validate array items are non-empty strings
        local items
        items=$(yq '.preferred_agents[]' "$config_file" 2>/dev/null)

        while IFS= read -r item; do
            [[ -z "$item" ]] && continue
            # Check item is a valid agent name format (alphanumeric, hyphens, underscores)
            # Also allow empty items (filtered out later)
            if [[ "$item" =~ ^[[:space:]]*$ ]]; then
                continue
            fi
            if [[ ! "$item" =~ ^[a-zA-Z0-9_-]+$ ]]; then
                _schema_error "preferred_agents: invalid agent name '$item' (must be alphanumeric with hyphens/underscores)"
                return 1
            fi
        done <<< "$items"
        return 0
    fi
    # No way to validate, assume ok
    return 0
}

# ============================================================================
# validate_workspace_config
# ============================================================================
# Validate a workspace config file (.needle.yaml).
# This is a subset of full config validation for workspace-specific configs.
#
# Usage: validate_workspace_config <workspace_config_file>
# Returns: 0 if valid, 1 if invalid
validate_workspace_config() {
    local config_file="${1:-}"

    if [[ -z "$config_file" ]] || [[ ! -f "$config_file" ]]; then
        return 0
    fi

    local errors=0

    # Check YAML syntax
    if command -v yq &>/dev/null; then
        if ! yq eval '.' "$config_file" &>/dev/null; then
            _schema_error "Invalid YAML syntax in workspace config: $config_file"
            return 1
        fi
    elif python3 -c "import yaml" 2>/dev/null; then
        if ! python3 -c "import yaml; yaml.safe_load(open('$config_file'))" 2>/dev/null; then
            _schema_error "Invalid YAML syntax in workspace config: $config_file"
            return 1
        fi
    fi

    # Warn if workspace tries to set strand list (strands are global only)
    local has_strands
    has_strands=$(_schema_get_value "$config_file" "strands")
    if [[ -n "$has_strands" ]]; then
        _schema_warn "strands: strand list is global only (set in ~/.needle/config.yaml, not workspace config)"
    fi

    # Validate preferred_agents if present
    if ! validate_preferred_agents "$config_file"; then
        ((errors++))
    fi

    [[ "$errors" -eq 0 ]]
}
