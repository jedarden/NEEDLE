#!/usr/bin/env bash
# NEEDLE Bug Scanner Module
# Integrates ultimate_bug_scanner (UBS) as a quality gate for beads
#
# This module handles:
# - Running UBS scans on bead working directories
# - Parsing scan results and determining pass/fail
# - Recording scan metrics in effort tracking
# - Configurable severity thresholds

# Source dependencies if not already loaded
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

if [[ -z "${_NEEDLE_EFFORT_LOADED:-}" ]]; then
    if [[ -f "$(dirname "${BASH_SOURCE[0]}")/../telemetry/effort.sh" ]]; then
        source "$(dirname "${BASH_SOURCE[0]}")/../telemetry/effort.sh"
    fi
fi

# Module version
_NFEDLE_BUG_SCANNER_VERSION="1.0.0"
_NFEDLE_BUG_SCANNER_LOADED=1

# -----------------------------------------------------------------------------
# Configuration
# -----------------------------------------------------------------------------

# Default configuration (can be overridden by NEEDLE_CONFIG)
BUG_SCANNER_ENABLED="${BUG_SCANNER_ENABLED:-true}"
BUG_SCANNER_SEVERITY_THRESHOLD="${BUG_SCANNER_SEVERITY_THRESHOLD:-error}"
BUG_SCANNER_FAIL_ON_ISSUES="${BUG_SCANNER_FAIL_ON_ISSUES:-true}"
BUG_SCANNER_TIMEOUT="${BUG_SCANNER_TIMEOUT:-300}"
BUG_SCANNER_OUTPUT_FORMAT="${BUG_SCANNER_OUTPUT_FORMAT:-json}"

# -----------------------------------------------------------------------------
# Utility Functions
# -----------------------------------------------------------------------------

# Check if UBS is available
# Returns: 0 if available, 1 if not
_bug_scanner_available() {
    command -v ubs &>/dev/null
}

# Ensure UBS is installed
# Returns: 0 if installed or successfully installs, 1 otherwise
_bug_scanner_ensure_installed() {
    if _bug_scanner_available; then
        return 0
    fi

    _needle_warn "ultimate_bug_scanner (UBS) not found, attempting installation..."

    # Try to use the NEEDLE bootstrap installer
    local bootstrap_script
    bootstrap_script="$(dirname "${BASH_SOURCE[0]}")/../../bootstrap/install.sh"

    if [[ -f "$bootstrap_script" ]]; then
        if bash "$bootstrap_script" --dep ubs 2>/dev/null; then
            # Setup PATH
            source "$bootstrap_script"
            if _bug_scanner_available; then
                _needle_info "UBS installed successfully"
                return 0
            fi
        fi
    fi

    # Fallback: direct install
    _needle_warn "Attempting direct UBS installation..."
    local cache_dir="${NEEDLE_CACHE_DIR:-$HOME/.needle/cache}"
    mkdir -p "$cache_dir"

    if curl -fsSL "https://raw.githubusercontent.com/Dicklesworthstone/ultimate_bug_scanner/master/install.sh" \
            NEEDLE_CACHE_DIR="$cache_dir" bash 2>/dev/null; then
        export PATH="$cache_dir:$PATH"
        if _bug_scanner_available; then
            _needle_info "UBS installed successfully to $cache_dir"
            return 0
        fi
    fi

    _needle_error "Failed to install UBS. Bug scanning disabled."
    return 1
}

# Convert severity name to numeric level for comparison
# Usage: _bug_scanner_severity_level <severity>
# Outputs: numeric level (3=critical, 2=error, 1=warning, 0=info)
_bug_scanner_severity_level() {
    local severity="$1"
    local lower_sev
    lower_sev=$(echo "$severity" | tr '[:upper:]' '[:lower:]')

    case "$lower_sev" in
        critical) echo 3 ;;
        error)    echo 2 ;;
        warning)  echo 1 ;;
        info|note) echo 0 ;;
        *)        echo 0 ;;
    esac
}

# Check if a finding meets the severity threshold
# Usage: _bug_scanner_meets_threshold <finding_severity> [threshold]
# Returns: 0 if meets threshold, 1 if below threshold
_bug_scanner_meets_threshold() {
    local finding_severity="$1"
    local threshold="${2:-$BUG_SCANNER_SEVERITY_THRESHOLD}"

    local finding_level
    local threshold_level

    finding_level=$(_bug_scanner_severity_level "$finding_severity")
    threshold_level=$(_bug_scanner_severity_level "$threshold")

    [[ $finding_level -ge $threshold_level ]]
}

# -----------------------------------------------------------------------------
# Scanning Functions
# -----------------------------------------------------------------------------

# Run UBS scan on a directory
# Usage: _bug_scanner_run_scan <directory> [output_file]
# Returns: UBS exit code (0=success, 1=issues found, 2=error)
# Outputs: JSON results to output_file if specified, else to stdout
_bug_scanner_run_scan() {
    local target_dir="$1"
    local output_file="${2:-}"

    if [[ ! -d "$target_dir" ]]; then
        _needle_error "Cannot scan: directory not found: $target_dir"
        return 2
    fi

    if ! _bug_scanner_available; then
        _needle_error "UBS not available"
        return 2
    fi

    # Build UBS command
    local cmd=(ubs)
    cmd+=("--format=$BUG_SCANNER_OUTPUT_FORMAT")
    cmd+=("--ci")

    # Set timeout
    cmd+=("--timeout=$BUG_SCANNER_TIMEOUT")

    # Add target directory
    cmd+=("$target_dir")

    # Run scan
    if [[ -n "$output_file" ]]; then
        "${cmd[@]}" > "$output_file" 2>&1
    else
        "${cmd[@]}" 2>&1
    fi

    # UBS exit codes: 0=success/no issues, 1=issues found, 2=error
    return $?
}

# Parse UBS JSON output and extract metrics
# Usage: _bug_scanner_parse_results <json_file>
# Outputs: Tab-separated values: total_count critical_count error_count warning_count info_count
_bug_scanner_parse_results() {
    local json_file="$1"

    if [[ ! -f "$json_file" ]]; then
        echo "0	0	0	0	0"
        return
    fi

    if ! command -v jq &>/dev/null; then
        _needle_warn "jq not available, cannot parse UBS results"
        echo "0	0	0	0	0"
        return
    fi

    # Use a file to avoid quoting issues
    local jq_script
    jq_script=$(cat <<'JQ_SCRIPT'
if .findings then
    .findings | map(.severity // "info") |
    ([.[] | select(. == "critical" or . == "security")] | length) as $crit |
    ([.[] | select(. == "error")] | length) as $err |
    ([.[] | select(. == "warning")] | length) as $warn |
    ([.[] | select(. == "info" or . == "note")] | length) as $info |
    ($crit + $err + $warn + $info) as $total |
    "\($total)\t\($crit)\t\($err)\t\($warn)\t\($info)"
else
    "0\t0\t0\t0\t0"
end
JQ_SCRIPT
)

    jq -r "$jq_script" "$json_file" 2>/dev/null || echo "0	0	0	0	0"
}

# Check if scan results indicate failure based on threshold
# Usage: _bug_scanner_should_fail <json_file> [threshold]
# Returns: 0 if should fail, 1 if should pass
_bug_scanner_should_fail() {
    local json_file="$1"
    local threshold="${2:-$BUG_SCANNER_SEVERITY_THRESHOLD}"

    if ! command -v jq &>/dev/null; then
        return 1
    fi

    # Use a file to avoid quoting issues
    local jq_script
    jq_script=$(cat <<'JQ_SCRIPT'
if .findings then
    .findings | map(.severity // "info") |
    map(select(
        . == "critical" or . == "security" or
        (. == "error" and ($threshold == "error" or $threshold == "warning" or $threshold == "info")) or
        (. == "warning" and ($threshold == "warning" or $threshold == "info")) or
        (. == "info" and $threshold == "info")
    )) | length
else
    0
end
JQ_SCRIPT
)

    local findings_above
    findings_above=$(jq -r --arg threshold "$threshold" "$jq_script" "$json_file" 2>/dev/null)

    [[ "${findings_above:-0}" -gt 0 ]]
}

# -----------------------------------------------------------------------------
# Main API Functions
# -----------------------------------------------------------------------------

# Scan a bead's working directory
# Usage: bug_scanner_scan_bead <bead_id> <working_directory>
# Returns: 0 if scan passed/no critical issues, 1 if failed, 2 if error
# Records metrics to effort tracking
bug_scanner_scan_bead() {
    local bead_id="$1"
    local work_dir="$2"

    # Check if enabled
    if [[ "$BUG_SCANNER_ENABLED" != "true" ]]; then
        _needle_debug "Bug scanner disabled, skipping scan for bead $bead_id"
        return 0
    fi

    # Ensure UBS is available
    if ! _bug_scanner_ensure_installed; then
        _needle_warn "UBS not available, skipping bug scan for bead $bead_id"
        return 0
    fi

    _needle_info "Running bug scan for bead $bead_id in $work_dir"

    # Create temp file for results
    local results_file
    results_file=$(mktemp)

    # Run scan
    local scan_start
    scan_start=$(date +%s)
    _bug_scanner_run_scan "$work_dir" "$results_file"
    local scan_exit=$?
    local scan_end
    scan_end=$(date +%s)
    local scan_duration=$((scan_end - scan_start))

    # Parse results
    local counts
    local total crit err warn info
    counts=$(_bug_scanner_parse_results "$results_file")
    IFS=$'\t' read -r total crit err warn info <<< "$counts"

    _needle_info "Scan results: ${total:-0} findings - critical: ${crit:-0}, error: ${err:-0}, warning: ${warn:-0}, info: ${info:-0}"

    # Record scan metrics
    if declare -f record_effort &>/dev/null; then
        record_effort "$bead_id" "0" "bug_scanner" "0" "0" 2>/dev/null || true
    fi

    # Store results file for later reference
    local results_cache_dir="${NEEDLE_CACHE_DIR:-$HOME/.needle/cache}/bug_scans"
    mkdir -p "$results_cache_dir"
    mv "$results_file" "$results_cache_dir/${bead_id}.json" 2>/dev/null || rm -f "$results_file"

    local results_file_final="$results_cache_dir/${bead_id}.json"

    # Determine pass/fail
    local should_fail=0
    if [[ "$BUG_SCANNER_FAIL_ON_ISSUES" == "true" ]]; then
        if _bug_scanner_should_fail "$results_file_final"; then
            should_fail=1
        fi
    fi

    # Create follow-up beads if issues found
    if [[ $should_fail -eq 1 ]] && [[ "${total:-0}" -gt 0 ]]; then
        _needle_warn "Bug scan found issues that exceed threshold '$BUG_SCANNER_SEVERITY_THRESHOLD'"

        # Optionally create follow-up bead for fixing issues
        if [[ "${BUG_SCANNER_CREATE_FOLLOW_UP:-true}" == "true" ]] && declare -f _needle_create_bead &>/dev/null; then
            local follow_up_title="Fix bugs found by scanner for $bead_id"
            local follow_up_desc
            follow_up_desc="Bug scan found ${crit:-0} critical, ${err:-0} error, and ${warn:-0} warning issues in bead $bead_id.

Severity threshold: $BUG_SCANNER_SEVERITY_THRESHOLD

Full results cached at: $results_file_final"

            _needle_create_bead \
                --type "bugfix" \
                --title "$follow_up_title" \
                --description "$follow_up_desc" \
                --parent "$bead_id" 2>/dev/null || true
        fi
    fi

    return $should_fail
}

# Quick check function for pre-flight validation
# Usage: bug_scanner_quick_check <directory>
# Returns: 0 if no critical issues, 1 if critical issues found, 2 if error
bug_scanner_quick_check() {
    local target_dir="$1"

    if ! _bug_scanner_available; then
        return 0
    fi

    local results_file
    results_file=$(mktemp)

    # Run with critical-only threshold
    _bug_scanner_run_scan "$target_dir" "$results_file"
    local scan_exit=$?

    # Check for critical issues only
    if _bug_scanner_should_fail "$results_file" "critical"; then
        rm -f "$results_file"
        return 1
    fi

    rm -f "$results_file"
    return 0
}

# Get bug scanner status
# Usage: bug_scanner_status
# Outputs: JSON status with version, availability, and configuration
bug_scanner_status() {
    local available="false"
    local version=""

    if _bug_scanner_available; then
        available="true"
        version=$(ubs --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+([.][0-9]+)?' | head -1 || echo "unknown")
    fi

    cat <<EOF
{
  "enabled": $BUG_SCANNER_ENABLED,
  "available": $available,
  "version": "$version",
  "severity_threshold": "$BUG_SCANNER_SEVERITY_THRESHOLD",
  "fail_on_issues": $BUG_SCANNER_FAIL_ON_ISSUES,
  "timeout": $BUG_SCANNER_TIMEOUT,
  "output_format": "$BUG_SCANNER_OUTPUT_FORMAT"
}
EOF
}

# -----------------------------------------------------------------------------
# Main (for direct execution)
# -----------------------------------------------------------------------------

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-}" in
        --scan)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 --scan <directory> [bead_id]" >&2
                exit 1
            fi
            bug_scanner_scan_bead "${3:-manual}" "$2"
            ;;
        --check)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 --check <directory>" >&2
                exit 1
            fi
            bug_scanner_quick_check "$2"
            ;;
        --status)
            bug_scanner_status
            ;;
        --install)
            _bug_scanner_ensure_installed
            ;;
        *)
            echo "NEEDLE Bug Scanner Module"
            echo ""
            echo "Usage: $0 <command> [args]"
            echo ""
            echo "Commands:"
            echo "  --scan <dir> [bead_id]   Scan directory and record as bead effort"
            echo "  --check <dir>             Quick check for critical issues"
            echo "  --status                  Show scanner status and configuration"
            echo "  --install                 Ensure UBS is installed"
            echo ""
            echo "Environment:"
            echo "  BUG_SCANNER_ENABLED           Enable/disable scanner (default: true)"
            echo "  BUG_SCANNER_SEVERITY_THRESHOLD Minimum severity (error, warning, info)"
            echo "  BUG_SCANNER_FAIL_ON_ISSUES    Fail beads on issues (default: true)"
            ;;
    esac
fi
