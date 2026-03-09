#!/usr/bin/env bash
# NEEDLE Hook Validation Module
# Validate hook scripts before execution
#
# Provides validation functions for:
#   - Executable bit on hook scripts
#   - Hook exit code handling (0, 1, 2, 3, 124)
#   - Environment variable injection
#   - Bash syntax checking
#
# Usage:
#   source src/hooks/validate.sh
#   _needle_validate_hook_script "/path/to/hook.sh"
#   _needle_validate_all_configured_hooks

# ============================================================================
# Executable Validation
# ============================================================================

# Validate that a hook script file is executable
# Usage: _needle_validate_hook_executable <path>
# Returns: 0 if executable, 1 if not
_needle_validate_hook_executable() {
    local path="$1"

    if [[ ! -f "$path" ]]; then
        _needle_warn "Hook file not found: $path"
        return 1
    fi

    if [[ ! -r "$path" ]]; then
        _needle_warn "Hook file not readable: $path"
        return 1
    fi

    if [[ ! -x "$path" ]]; then
        _needle_warn "Hook file not executable: $path (run: chmod +x $path)"
        return 1
    fi

    return 0
}

# ============================================================================
# Syntax Validation
# ============================================================================

# Check a hook script for bash syntax errors
# Usage: _needle_validate_hook_syntax <path>
# Returns: 0 if syntax is valid, 1 if errors found
_needle_validate_hook_syntax() {
    local path="$1"

    if [[ ! -f "$path" ]]; then
        _needle_warn "Hook file not found for syntax check: $path"
        return 1
    fi

    # Detect interpreter from shebang
    local shebang
    shebang=$(head -1 "$path" 2>/dev/null || echo "")

    local syntax_errors
    if echo "$shebang" | grep -qE "bash|sh"; then
        syntax_errors=$(bash -n "$path" 2>&1)
        local exit_code=$?
        if [[ $exit_code -ne 0 ]]; then
            _needle_warn "Hook syntax error in $path: $syntax_errors"
            return 1
        fi
    else
        _needle_debug "Non-bash hook, skipping syntax check: $path"
    fi

    return 0
}

# ============================================================================
# Exit Code Validation
# ============================================================================

# Validate that a hook exit code is a known/handled code
# Usage: _needle_validate_hook_exit_code <exit_code>
# Returns: 0 if code is recognized, 1 if unknown
_needle_validate_hook_exit_code() {
    local exit_code="$1"

    case "$exit_code" in
        0)   # Success
            return 0
            ;;
        1)   # Warning
            return 0
            ;;
        2)   # Abort
            return 0
            ;;
        3)   # Skip
            return 0
            ;;
        124) # Timeout
            return 0
            ;;
        *)
            _needle_warn "Unrecognized hook exit code: $exit_code (expected: 0=success, 1=warning, 2=abort, 3=skip)"
            return 1
            ;;
    esac
}

# Validate that all exit codes (0, 1, 2, 3) are handled by runner
# This is a self-check to verify the runner's exit code handling is complete
# Usage: _needle_validate_exit_code_coverage
# Returns: 0 if all codes handled, 1 if any missing
_needle_validate_exit_code_coverage() {
    local missing=()
    local required_codes=(0 1 2 3 124)

    for code in "${required_codes[@]}"; do
        _needle_validate_hook_exit_code "$code" 2>/dev/null || missing+=("$code")
    done

    if [[ ${#missing[@]} -gt 0 ]]; then
        _needle_warn "Hook exit code coverage gap: codes ${missing[*]} not handled"
        return 1
    fi

    return 0
}

# ============================================================================
# Environment Variable Validation
# ============================================================================

# Check that required NEEDLE environment variables are set for hook execution
# Usage: _needle_validate_hook_env [bead_id]
# Returns: 0 if all required vars present, 1 if missing
_needle_validate_hook_env() {
    local bead_id="${1:-}"
    local missing=()

    # Check core variables that must be set before hooks run
    [[ -n "${NEEDLE_HOME:-}" ]]        || missing+=("NEEDLE_HOME")
    [[ -n "${NEEDLE_CONFIG_FILE:-}" ]] || missing+=("NEEDLE_CONFIG_FILE")

    if [[ ${#missing[@]} -gt 0 ]]; then
        _needle_warn "Missing required hook environment variables: ${missing[*]}"
        return 1
    fi

    # Check hook-specific variables (set by _needle_set_hook_env)
    local hook_vars_missing=()
    [[ -n "${NEEDLE_HOOK:-}" ]]      || hook_vars_missing+=("NEEDLE_HOOK")
    [[ -n "${NEEDLE_PID:-}" ]]       || hook_vars_missing+=("NEEDLE_PID")
    [[ -n "${NEEDLE_WORKSPACE:-}" ]] || hook_vars_missing+=("NEEDLE_WORKSPACE")

    if [[ ${#hook_vars_missing[@]} -gt 0 ]]; then
        _needle_debug "Hook-specific env vars not yet set (expected before execution): ${hook_vars_missing[*]}"
        # Not a hard failure — these are set just before hook runs
    fi

    return 0
}

# Validate that a hook script references only known NEEDLE_ environment variables
# Usage: _needle_validate_hook_env_vars <path>
# Returns: 0 if only known vars used, 1 if unknown vars detected
_needle_validate_hook_env_vars() {
    local path="$1"

    if [[ ! -f "$path" ]]; then
        _needle_warn "Hook file not found for env var check: $path"
        return 1
    fi

    # Known NEEDLE_ environment variables available to hooks
    local known_vars=(
        NEEDLE_HOOK
        NEEDLE_BEAD_ID
        NEEDLE_BEAD_TITLE
        NEEDLE_BEAD_PRIORITY
        NEEDLE_BEAD_TYPE
        NEEDLE_BEAD_LABELS
        NEEDLE_WORKER
        NEEDLE_SESSION
        NEEDLE_PID
        NEEDLE_WORKSPACE
        NEEDLE_AGENT
        NEEDLE_STRAND
        NEEDLE_STRAND_NAME
        NEEDLE_EXIT_CODE
        NEEDLE_DURATION_MS
        NEEDLE_OUTPUT_FILE
        NEEDLE_FILES_CHANGED
        NEEDLE_LINES_ADDED
        NEEDLE_LINES_REMOVED
        NEEDLE_HOOK_CONFIG_FILE
        NEEDLE_HOOK_HOME
        NEEDLE_HOME
        NEEDLE_CONFIG_FILE
        NEEDLE_VERBOSE
        NEEDLE_QUIET
    )

    # Extract all NEEDLE_ variable references from the script
    local used_vars
    used_vars=$(grep -oE '\$\{?NEEDLE_[A-Z_]+\}?' "$path" 2>/dev/null | \
        sed 's/[${}]//g' | sort -u)

    local unknown=()
    while IFS= read -r var; do
        [[ -z "$var" ]] && continue
        local found=false
        for known in "${known_vars[@]}"; do
            if [[ "$var" == "$known" ]]; then
                found=true
                break
            fi
        done
        [[ "$found" == "false" ]] && unknown+=("$var")
    done <<< "$used_vars"

    if [[ ${#unknown[@]} -gt 0 ]]; then
        _needle_warn "Hook $path uses unknown NEEDLE_ variables: ${unknown[*]}"
        # Return warning (not hard failure — custom vars may be intentional)
        return 1
    fi

    return 0
}

# ============================================================================
# Script-Level Validation
# ============================================================================

# Validate a single hook script comprehensively
# Usage: _needle_validate_hook_script <path> [--strict]
# Returns: 0 if valid, 1 if issues found
# With --strict: unknown env vars also cause failure
_needle_validate_hook_script() {
    local path="$1"
    local strict="${2:-}"
    local has_errors=false

    # Expand ~ to home directory
    path="${path/#\~/$HOME}"

    # Handle workspace-relative paths
    if [[ -n "${NEEDLE_WORKSPACE:-}" ]] && [[ "$path" == ./* ]]; then
        path="${NEEDLE_WORKSPACE}/${path#./}"
    fi

    if [[ ! -e "$path" ]]; then
        _needle_warn "Hook script does not exist: $path"
        return 1
    fi

    # Check executable
    if ! _needle_validate_hook_executable "$path"; then
        has_errors=true
    fi

    # Check syntax
    if ! _needle_validate_hook_syntax "$path"; then
        has_errors=true
    fi

    # Check env vars (non-fatal unless --strict)
    if [[ "$strict" == "--strict" ]]; then
        if ! _needle_validate_hook_env_vars "$path" 2>/dev/null; then
            has_errors=true
        fi
    else
        _needle_validate_hook_env_vars "$path" 2>/dev/null || true
    fi

    [[ "$has_errors" == "false" ]]
}

# ============================================================================
# Bulk Validation
# ============================================================================

# Validate all hook scripts configured in the current config
# Usage: _needle_validate_all_configured_hooks [--strict]
# Returns: 0 if all valid, 1 if any issues found
_needle_validate_all_configured_hooks() {
    local strict="${1:-}"
    local has_errors=false

    # Check base environment
    if ! _needle_validate_hook_env; then
        has_errors=true
    fi

    # Validate exit code coverage
    if ! _needle_validate_exit_code_coverage 2>/dev/null; then
        has_errors=true
    fi

    # Validate timeout format
    local timeout
    timeout=$(get_config "hooks.timeout" "30s")
    if [[ ! "$timeout" =~ ^[0-9]+s?$ ]]; then
        _needle_warn "Invalid hooks.timeout format: $timeout (expected: Ns or N)"
        has_errors=true
    fi

    # Validate fail_action value
    local fail_action
    fail_action=$(get_config "hooks.fail_action" "warn")
    case "$fail_action" in
        warn|abort|ignore) ;;
        *)
            _needle_warn "Invalid hooks.fail_action: $fail_action (expected: warn, abort, or ignore)"
            has_errors=true
            ;;
    esac

    # Validate each configured hook script
    if [[ -z "${NEEDLE_HOOK_TYPES[*]:-}" ]]; then
        _needle_warn "NEEDLE_HOOK_TYPES not set — source hooks/runner.sh first"
        return 1
    fi

    for hook_type in "${NEEDLE_HOOK_TYPES[@]}"; do
        local hook_path
        hook_path=$(get_config "hooks.$hook_type" "")

        [[ -z "$hook_path" ]] && continue

        _needle_debug "Validating hook: $hook_type -> $hook_path"

        if ! _needle_validate_hook_script "$hook_path" "$strict"; then
            _needle_warn "Hook validation failed: $hook_type ($hook_path)"
            has_errors=true
        fi
    done

    [[ "$has_errors" == "false" ]]
}

# Print a human-readable validation report for all configured hooks
# Usage: _needle_hook_validation_report
_needle_hook_validation_report() {
    _needle_section "Hook Validation Report"

    local timeout fail_action
    timeout=$(get_config "hooks.timeout" "30s")
    fail_action=$(get_config "hooks.fail_action" "warn")

    _needle_table_row "timeout" "$timeout"
    _needle_table_row "fail_action" "$fail_action"
    _needle_print ""

    local any_configured=false

    for hook_type in "${NEEDLE_HOOK_TYPES[@]}"; do
        local hook_path
        hook_path=$(get_config "hooks.$hook_type" "")

        [[ -z "$hook_path" ]] && continue
        any_configured=true

        local expanded="${hook_path/#\~/$HOME}"
        if [[ -n "${NEEDLE_WORKSPACE:-}" ]] && [[ "$expanded" == ./* ]]; then
            expanded="${NEEDLE_WORKSPACE}/${expanded#./}"
        fi

        local issues=()

        if [[ ! -f "$expanded" ]]; then
            issues+=("not found")
        else
            [[ ! -r "$expanded" ]] && issues+=("not readable")
            [[ ! -x "$expanded" ]] && issues+=("not executable")

            # Syntax check
            local shebang
            shebang=$(head -1 "$expanded" 2>/dev/null || echo "")
            if echo "$shebang" | grep -qE "bash|sh"; then
                if ! bash -n "$expanded" 2>/dev/null; then
                    issues+=("syntax error")
                fi
            fi
        fi

        if [[ ${#issues[@]} -eq 0 ]]; then
            printf '  %-16s %s %s\n' "$hook_type" "$hook_path" \
                "${NEEDLE_COLOR_GREEN:-}✓ ok${NEEDLE_COLOR_RESET:-}"
        else
            printf '  %-16s %s %s\n' "$hook_type" "$hook_path" \
                "${NEEDLE_COLOR_RED:-}✗ ${issues[*]}${NEEDLE_COLOR_RESET:-}"
        fi
    done

    if [[ "$any_configured" == "false" ]]; then
        _needle_print "  No hooks configured"
    fi
}
