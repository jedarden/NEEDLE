#!/usr/bin/env bash
# NEEDLE Strand: pulse (Priority 6)
# Codebase health monitoring
#
# Implementation: nd-2oy
#
# This strand monitors codebase health metrics including:
# - Security vulnerabilities (scan detector)
# - Dependency freshness (version detector)
# - Documentation drift (doc detector)
# - Test coverage trends (coverage detector)
#
# The strand runs periodically based on frequency configuration and
# creates beads for detected issues up to a configurable limit.
#
# Usage:
#   _needle_strand_pulse <workspace> <agent>
#
# Return values:
#   0 - Work was found and processed (beads created)
#   1 - No work found (fallthrough to next strand)

# Source diagnostic module if not already loaded
if [[ -z "${_NEEDLE_DIAGNOSTIC_LOADED:-}" ]]; then
    NEEDLE_SRC="${NEEDLE_SRC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
    source "$NEEDLE_SRC/lib/diagnostic.sh"
fi

# Source bead claim module for _needle_create_bead
if [[ -z "${_NEEDLE_CLAIM_LOADED:-}" ]]; then
    NEEDLE_SRC="${NEEDLE_SRC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
    source "$NEEDLE_SRC/bead/claim.sh"
fi

# ============================================================================
# Pulse State Directory
# ============================================================================

# Get the pulse state directory path
# Usage: _pulse_state_dir
# Returns: Path to pulse state directory
_pulse_state_dir() {
    echo "$NEEDLE_HOME/$NEEDLE_STATE_DIR/pulse"
}

# Ensure pulse state directory exists
# Usage: _pulse_ensure_state_dir
_pulse_ensure_state_dir() {
    local state_dir
    state_dir=$(_pulse_state_dir)
    mkdir -p "$state_dir"
}

# ============================================================================
# Duration Parsing
# ============================================================================

# Parse duration string to seconds
# Supports: s (seconds), m (minutes), h (hours), d (days)
# Examples: "30s", "5m", "2h", "1d", "24h"
#
# Usage: _pulse_parse_duration <duration_string>
# Returns: Duration in seconds
_pulse_parse_duration() {
    local duration="$1"

    # Default to 24 hours if empty
    if [[ -z "$duration" ]]; then
        echo 86400
        return 0
    fi

    local value="${duration%[smhd]}"
    local unit="${duration: -1}"

    # Validate value is numeric
    if [[ ! "$value" =~ ^[0-9]+$ ]]; then
        echo 86400  # Default to 24h on parse error
        return 1
    fi

    case "$unit" in
        s) echo "$value" ;;
        m) echo $((value * 60)) ;;
        h) echo $((value * 3600)) ;;
        d) echo $((value * 86400)) ;;
        *)
            # Assume seconds if no unit
            if [[ "$duration" =~ ^[0-9]+$ ]]; then
                echo "$duration"
            else
                echo 86400
            fi
            ;;
    esac
}

# ============================================================================
# Frequency Checking
# ============================================================================

# Check if pulse should run based on frequency configuration
# Returns: 0 if should run, 1 if rate limited (too soon)
_pulse_should_run() {
    local workspace="$1"

    # Get frequency from config (default: 24 hours)
    local freq
    freq=$(get_config "strands.pulse.frequency" "24h")

    local freq_seconds
    freq_seconds=$(_pulse_parse_duration "$freq")

    # Create workspace-specific state
    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir
    state_dir=$(_pulse_state_dir)
    local last_scan_file="$state_dir/last_scan_${workspace_hash}.json"

    _pulse_ensure_state_dir

    # Check if last scan file exists
    if [[ -f "$last_scan_file" ]]; then
        local last_scan
        last_scan=$(jq -r '.last_scan // 0' "$last_scan_file" 2>/dev/null)

        if [[ -n "$last_scan" ]] && [[ "$last_scan" =~ ^[0-9]+$ ]] && [[ "$last_scan" -gt 0 ]]; then
            local now
            now=$(date +%s)
            local elapsed=$((now - last_scan))

            if ((elapsed < freq_seconds)); then
                _needle_diag_strand "pulse" "Frequency limit not reached" \
                    "workspace=$workspace" \
                    "elapsed=${elapsed}s" \
                    "required=${freq_seconds}s" \
                    "remaining=$((freq_seconds - elapsed))s"

                _needle_verbose "pulse: rate limited (${elapsed}s since last scan, need ${freq_seconds}s)"
                return 1
            fi
        fi
    fi

    _needle_diag_strand "pulse" "Frequency check passed" \
        "workspace=$workspace" \
        "frequency=$freq" \
        "frequency_seconds=$freq_seconds"

    return 0
}

# ============================================================================
# State Management
# ============================================================================

# Get pulse state value
# Usage: _pulse_get_state <workspace> <key>
# Returns: State value or empty string
_pulse_get_state() {
    local workspace="$1"
    local key="$2"

    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir
    state_dir=$(_pulse_state_dir)
    local state_file="$state_dir/state_${workspace_hash}.json"

    if [[ ! -f "$state_file" ]]; then
        return 1
    fi

    jq -r ".$key // empty" "$state_file" 2>/dev/null
}

# Set pulse state value
# Usage: _pulse_set_state <workspace> <key> <value>
_pulse_set_state() {
    local workspace="$1"
    local key="$2"
    local value="$3"

    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir
    state_dir=$(_pulse_state_dir)
    local state_file="$state_dir/state_${workspace_hash}.json"

    _pulse_ensure_state_dir

    # Initialize file if it doesn't exist
    if [[ ! -f "$state_file" ]]; then
        echo '{}' > "$state_file"
    fi

    # Update state using jq
    local tmp_file
    tmp_file=$(mktemp)
    if jq --arg k "$key" --arg v "$value" '. + {($k): $v}' "$state_file" > "$tmp_file" 2>/dev/null; then
        mv "$tmp_file" "$state_file"
    else
        rm -f "$tmp_file"
        return 1
    fi
}

# Record pulse scan completion
# Usage: _pulse_record_scan <workspace>
_pulse_record_scan() {
    local workspace="$1"

    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir
    state_dir=$(_pulse_state_dir)
    local last_scan_file="$state_dir/last_scan_${workspace_hash}.json"

    _pulse_ensure_state_dir

    local now
    now=$(date +%s)

    # Write last scan timestamp
    cat > "$last_scan_file" << EOF
{
  "last_scan": $now,
  "last_scan_iso": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "workspace": "$workspace"
}
EOF

    _needle_diag_strand "pulse" "Recorded scan completion" \
        "workspace=$workspace" \
        "timestamp=$now"
}

# ============================================================================
# Issue Deduplication (Fingerprinting)
# ============================================================================

# Get the seen issues file path
# Usage: _pulse_seen_file <workspace>
_pulse_seen_file() {
    local workspace="$1"
    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)
    echo "$(_pulse_state_dir)/seen_issues_${workspace_hash}.jsonl"
}

# Check if an issue has already been seen (deduplication)
# Uses fingerprint hash to identify duplicate issues
#
# Usage: _pulse_already_seen <workspace> <fingerprint>
# Returns: 0 if already seen, 1 if new
_pulse_already_seen() {
    local workspace="$1"
    local fingerprint="$2"

    if [[ -z "$fingerprint" ]]; then
        return 1  # No fingerprint = treat as new
    fi

    local seen_file
    seen_file=$(_pulse_seen_file "$workspace")

    if [[ ! -f "$seen_file" ]]; then
        return 1  # No seen file = all issues are new
    fi

    # Create fingerprint hash for lookup
    local fp_hash
    fp_hash=$(echo -n "$fingerprint" | sha256sum | cut -c1-16)

    # Check if fingerprint exists in seen file
    if grep -q "\"fingerprint_hash\":\"$fp_hash\"" "$seen_file" 2>/dev/null; then
        _needle_debug "pulse: issue already seen (fingerprint: $fp_hash)"
        return 0
    fi

    return 1
}

# Mark an issue as seen
# Usage: _pulse_mark_seen <workspace> <fingerprint> <category> <title>
_pulse_mark_seen() {
    local workspace="$1"
    local fingerprint="$2"
    local category="$3"
    local title="$4"

    if [[ -z "$fingerprint" ]]; then
        return 0
    fi

    local seen_file
    seen_file=$(_pulse_seen_file "$workspace")

    _pulse_ensure_state_dir

    # Create fingerprint hash
    local fp_hash
    fp_hash=$(echo -n "$fingerprint" | sha256sum | cut -c1-16)

    local now
    now=$(date -u +%Y-%m-%dT%H:%M:%SZ)

    # Append to seen file
    local entry
    entry=$(jq -n \
        --arg fp_hash "$fp_hash" \
        --arg fingerprint "$fingerprint" \
        --arg category "$category" \
        --arg title "$title" \
        --arg seen_at "$now" \
        '{
            fingerprint_hash: $fp_hash,
            fingerprint: $fingerprint,
            category: $category,
            title: $title,
            seen_at: $seen_at
        }')

    echo "$entry" >> "$seen_file"

    _needle_diag_strand "pulse" "Marked issue as seen" \
        "workspace=$workspace" \
        "fingerprint_hash=$fp_hash" \
        "category=$category"
}

# Clean old seen issues (older than retention period)
# Usage: _pulse_clean_seen_issues <workspace> [retention_days]
_pulse_clean_seen_issues() {
    local workspace="$1"
    local retention_days="${2:-30}"

    local seen_file
    seen_file=$(_pulse_seen_file "$workspace")

    if [[ ! -f "$seen_file" ]]; then
        return 0
    fi

    # Calculate cutoff timestamp
    local cutoff_epoch
    cutoff_epoch=$(date -d "${retention_days} days ago" +%s 2>/dev/null || date -v-${retention_days}d +%s 2>/dev/null)

    if [[ -z "$cutoff_epoch" ]]; then
        return 0
    fi

    # Filter out old entries
    local tmp_file
    tmp_file=$(mktemp)
    local count=0

    while IFS= read -r line; do
        local seen_at
        seen_at=$(echo "$line" | jq -r '.seen_at // empty' 2>/dev/null)

        if [[ -n "$seen_at" ]]; then
            local seen_epoch
            seen_epoch=$(date -d "$seen_at" +%s 2>/dev/null || echo 0)

            if [[ "$seen_epoch" -ge "$cutoff_epoch" ]]; then
                echo "$line" >> "$tmp_file"
            else
                ((count++))
            fi
        fi
    done < "$seen_file"

    if [[ -f "$tmp_file" ]]; then
        mv "$tmp_file" "$seen_file"
    else
        rm -f "$tmp_file"
    fi

    if ((count > 0)); then
        _needle_debug "pulse: cleaned $count old seen issue(s)"
    fi
}

# ============================================================================
# Bead Creation Helper
# ============================================================================

# Create a bead for a detected pulse issue
# Handles deduplication and max beads limit
#
# Usage: _pulse_create_bead <workspace> <category> <title> <description> <fingerprint> [severity] [labels]
#
# Arguments:
#   workspace   - Workspace path
#   category    - Issue category (security, dependency, docs, coverage)
#   title       - Bead title
#   description - Full bead description
#   fingerprint - Unique fingerprint for deduplication
#   severity    - Severity level (critical, high, medium, low) - optional, defaults to medium
#   labels      - Comma-separated extra labels - optional
#
# Returns: 0 if bead created, 1 if skipped or failed
# Outputs: Created bead ID on success
_pulse_create_bead() {
    local workspace="$1"
    local category="$2"
    local title="$3"
    local description="$4"
    local fingerprint="$5"
    local severity="${6:-medium}"
    local extra_labels="${7:-}"

    # Check if already seen
    if _pulse_already_seen "$workspace" "$fingerprint"; then
        _needle_verbose "pulse: skipping duplicate issue: $title"
        return 1
    fi

    # Map severity to priority
    local priority=2  # Default: normal
    case "$severity" in
        critical) priority=0 ;;
        high)     priority=1 ;;
        medium)   priority=2 ;;
        low)      priority=3 ;;
    esac

    # Build labels
    local labels="pulse,$category,automated"
    if [[ -n "$extra_labels" ]]; then
        labels="$labels,$extra_labels"
    fi

    # Create the bead using wrapper (handles unassigned_by_default)
    local bead_id
    bead_id=$(_needle_create_bead \
        --workspace "$workspace" \
        --title "$title" \
        --description "$description" \
        --type task \
        --priority "$priority" \
        --label "$labels" \
        --silent 2>/dev/null)

    if [[ $? -eq 0 ]] && [[ -n "$bead_id" ]]; then
        # Mark as seen
        _pulse_mark_seen "$workspace" "$fingerprint" "$category" "$title"

        _needle_info "pulse: created bead: $bead_id - $title"

        # Emit telemetry event
        _needle_telemetry_emit "pulse.bead_created" "info" \
            "bead_id=$bead_id" \
            "category=$category" \
            "severity=$severity" \
            "title=$title" \
            "workspace=$workspace"

        echo "$bead_id"
        return 0
    else
        _needle_warn "pulse: failed to create bead: $title"
        return 1
    fi
}

# ============================================================================
# Security Vulnerability Detector (nd-21h)
# ============================================================================

# Map npm audit severity to standard severity
# Usage: _pulse_map_npm_severity <npm_severity>
# Returns: standard severity (critical, high, medium, low)
_pulse_map_npm_severity() {
    local npm_severity="$1"

    case "${npm_severity,,}" in
        critical) echo "critical" ;;
        high)     echo "high" ;;
        moderate) echo "medium" ;;
        low)      echo "low" ;;
        info)     echo "low" ;;
        *)        echo "medium" ;;
    esac
}

# Map pip-audit severity to standard severity
# pip-audit uses CVSS scores, so we map based on those
# Usage: _pulse_map_pip_severity <cvss_score>
# Returns: standard severity (critical, high, medium, low)
_pulse_map_pip_severity() {
    local cvss_score="$1"

    # Default to medium if score is missing or invalid
    if [[ -z "$cvss_score" ]] || ! [[ "$cvss_score" =~ ^[0-9.]+$ ]]; then
        echo "medium"
        return 0
    fi

    # CVSS v3.1 severity ratings using awk for floating-point comparison
    local severity
    severity=$(awk -v score="$cvss_score" 'BEGIN {
        if (score >= 9.0) print "critical"
        else if (score >= 7.0) print "high"
        else if (score >= 4.0) print "medium"
        else print "low"
    }')

    echo "$severity"
}

# Run npm audit and parse vulnerabilities
# Usage: _pulse_npm_audit <workspace>
# Returns: JSON array of vulnerability issue objects
_pulse_npm_audit() {
    local workspace="$1"
    local issues="[]"

    # Check for package.json
    if [[ ! -f "$workspace/package.json" ]]; then
        echo "[]"
        return 0
    fi

    # Check if npm is available
    if ! command -v npm &>/dev/null; then
        _needle_debug "pulse: npm not found, skipping Node.js vulnerability scan"
        echo "[]"
        return 0
    fi

    # Run npm audit in JSON format
    local audit_output
    audit_output=$(cd "$workspace" && npm audit --json 2>/dev/null) || {
        # npm audit returns non-zero when vulnerabilities are found
        # This is expected, so we continue processing
        :
    }

    # Check if we got valid JSON
    if [[ -z "$audit_output" ]] || ! echo "$audit_output" | jq -e . &>/dev/null; then
        _needle_debug "pulse: npm audit returned invalid JSON or empty output"
        echo "[]"
        return 0
    fi

    # Parse vulnerabilities from npm audit output
    # npm audit JSON format has a "vulnerabilities" object with package names as keys
    local vuln_packages
    vuln_packages=$(echo "$audit_output" | jq -r '.vulnerabilities | keys[]' 2>/dev/null)

    if [[ -z "$vuln_packages" ]]; then
        echo "[]"
        return 0
    fi

    while IFS= read -r pkg_name; do
        [[ -z "$pkg_name" ]] && continue

        # Get vulnerability details
        local vuln_info vuln_severity cve_ids advisory_url

        vuln_info=$(echo "$audit_output" | jq -c ".vulnerabilities[\"$pkg_name\"]" 2>/dev/null)
        vuln_severity=$(echo "$vuln_info" | jq -r '.severity // "moderate"' 2>/dev/null)
        cve_ids=$(echo "$vuln_info" | jq -r '.via[]? | select(type == "object") | .cwe // empty' 2>/dev/null | head -1)
        advisory_url=$(echo "$vuln_info" | jq -r '.via[]? | select(type == "object") | .url // empty' 2>/dev/null | head -1)

        # Map to standard severity
        local severity
        severity=$(_pulse_map_npm_severity "$vuln_severity")

        # Create fingerprint from package name and vulnerability info
        local fingerprint="npm:${pkg_name}:${vuln_severity}"

        # Build title and description
        local title="Fix security vulnerability in npm package: ${pkg_name}"
        local description="Security vulnerability detected in npm package **${pkg_name}**.

**Severity:** ${severity}
**NPM Severity:** ${vuln_severity}"

        if [[ -n "$cve_ids" ]]; then
            description+="
**CWE:** ${cve_ids}"
            fingerprint="npm:${pkg_name}:${cve_ids}"
        fi

        if [[ -n "$advisory_url" ]]; then
            description+="

**Advisory:** ${advisory_url}"
        fi

        description+="

## Remediation
Run \`npm audit fix\` to attempt automatic fixes, or manually update the package to a patched version."

        # Create issue object
        local issue
        issue=$(jq -n \
            --arg category "security" \
            --arg severity "$severity" \
            --arg title "$title" \
            --arg description "$description" \
            --arg fingerprint "$fingerprint" \
            --arg labels "npm,vulnerability" \
            '{
                category: $category,
                severity: $severity,
                title: $title,
                description: $description,
                fingerprint: $fingerprint,
                labels: $labels
            }')

        issues=$(echo "$issues" "$issue" | jq -s 'add' 2>/dev/null || echo "$issues")

    done <<< "$vuln_packages"

    echo "$issues"
}

# Run pip-audit and parse vulnerabilities
# Usage: _pulse_pip_audit <workspace>
# Returns: JSON array of vulnerability issue objects
_pulse_pip_audit() {
    local workspace="$1"
    local issues="[]"

    # Check for requirements.txt, pyproject.toml, or setup.py
    local has_python_reqs=false
    if [[ -f "$workspace/requirements.txt" ]] || \
       [[ -f "$workspace/pyproject.toml" ]] || \
       [[ -f "$workspace/setup.py" ]] || \
       [[ -f "$workspace/requirements-dev.txt" ]]; then
        has_python_reqs=true
    fi

    if [[ "$has_python_reqs" != "true" ]]; then
        echo "[]"
        return 0
    fi

    # Check if pip-audit is available
    if ! command -v pip-audit &>/dev/null; then
        _needle_debug "pulse: pip-audit not found, skipping Python vulnerability scan"
        echo "[]"
        return 0
    fi

    # Run pip-audit in JSON format
    local audit_output
    audit_output=$(cd "$workspace" && pip-audit --format json 2>/dev/null) || {
        # pip-audit returns non-zero when vulnerabilities are found
        :  # Continue processing
    }

    # Check if we got valid JSON
    if [[ -z "$audit_output" ]] || ! echo "$audit_output" | jq -e . &>/dev/null; then
        _needle_debug "pulse: pip-audit returned invalid JSON or empty output"
        echo "[]"
        return 0
    fi

    # Parse vulnerabilities from pip-audit output
    # pip-audit JSON format is an array of package vulnerability objects
    local vuln_count
    vuln_count=$(echo "$audit_output" | jq 'length' 2>/dev/null || echo 0)

    if [[ "$vuln_count" -eq 0 ]]; then
        echo "[]"
        return 0
    fi

    # Iterate through vulnerabilities
    local idx=0
    while ((idx < vuln_count)); do
        local vuln_info pkg_name pkg_version

        vuln_info=$(echo "$audit_output" | jq -c ".[$idx]" 2>/dev/null)
        pkg_name=$(echo "$vuln_info" | jq -r '.package.name // empty' 2>/dev/null)
        pkg_version=$(echo "$vuln_info" | jq -r '.package.version // "unknown"' 2>/dev/null)

        # Skip if no package name
        if [[ -z "$pkg_name" ]]; then
            ((idx++))
            continue
        fi

        # Process each vulnerability in the package
        local vulns_in_pkg vuln_idx
        vulns_in_pkg=$(echo "$vuln_info" | jq '.vulnerabilities | length' 2>/dev/null || echo 0)
        vuln_idx=0

        while ((vuln_idx < vulns_in_pkg)); do
            local vuln_detail cve_id cvss_score fix_versions advisory_url

            vuln_detail=$(echo "$vuln_info" | jq -c ".vulnerabilities[$vuln_idx]" 2>/dev/null)
            cve_id=$(echo "$vuln_detail" | jq -r '.id // empty' 2>/dev/null)
            cvss_score=$(echo "$vuln_detail" | jq -r '.cvss?.score // .severity // empty' 2>/dev/null)
            fix_versions=$(echo "$vuln_detail" | jq -r '.fix_versions | join(", ") // empty' 2>/dev/null)
            advisory_url=$(echo "$vuln_detail" | jq -r '.aliases[]? | select(startswith("PYSEC") or startswith("GHSA")) // empty' 2>/dev/null | head -1)

            # Map to standard severity
            local severity
            severity=$(_pulse_map_pip_severity "$cvss_score")

            # Create fingerprint
            local fingerprint="pip:${pkg_name}:${cve_id}"

            # Build title and description
            local title="Fix security vulnerability in Python package: ${pkg_name}"
            local description="Security vulnerability detected in Python package **${pkg_name}** (version ${pkg_version}).

**Severity:** ${severity}
**CVE:** ${cve_id}"

            if [[ -n "$advisory_url" ]]; then
                description+="
**Advisory:** ${advisory_url}"
            fi

            if [[ -n "$fix_versions" ]]; then
                description+="

## Remediation
Update to a patched version: ${fix_versions}

\`\`\`bash
pip install ${pkg_name}>=${fix_versions%%,*}
\`\`\`"
            else
                description+="

## Remediation
Check for a patched version of ${pkg_name} or consider replacing this dependency."
            fi

            # Create issue object
            local issue
            issue=$(jq -n \
                --arg category "security" \
                --arg severity "$severity" \
                --arg title "$title" \
                --arg description "$description" \
                --arg fingerprint "$fingerprint" \
                --arg labels "python,pip,vulnerability" \
                '{
                    category: $category,
                    severity: $severity,
                    title: $title,
                    description: $description,
                    fingerprint: $fingerprint,
                    labels: $labels
                }')

            issues=$(echo "$issues" "$issue" | jq -s 'add' 2>/dev/null || echo "$issues")

            ((vuln_idx++))
        done

        ((idx++))
    done

    echo "$issues"
}

# Main security vulnerability detector
# Scans for vulnerabilities in Node.js and Python dependencies
#
# Usage: _pulse_detector_security <workspace> <agent>
# Returns: JSON array of security issue objects
_pulse_detector_security() {
    local workspace="$1"
    local agent="$2"

    _needle_diag_strand "pulse" "Running security detector" \
        "workspace=$workspace" \
        "agent=$agent"

    # Emit detector started event
    _needle_telemetry_emit "pulse.detector_started" "info" \
        "detector=security" \
        "workspace=$workspace"

    local all_issues="[]"
    local issues_found=0

    # Run npm audit for Node.js projects
    local npm_issues
    npm_issues=$(_pulse_npm_audit "$workspace")
    if [[ -n "$npm_issues" ]] && [[ "$npm_issues" != "[]" ]]; then
        all_issues=$(echo "$all_issues" "$npm_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        local npm_count
        npm_count=$(echo "$npm_issues" | jq 'length' 2>/dev/null || echo 0)
        ((issues_found += npm_count))
        _needle_verbose "pulse: found $npm_count npm vulnerability(ies)"
    fi

    # Run pip-audit for Python projects
    local pip_issues
    pip_issues=$(_pulse_pip_audit "$workspace")
    if [[ -n "$pip_issues" ]] && [[ "$pip_issues" != "[]" ]]; then
        all_issues=$(echo "$all_issues" "$pip_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        local pip_count
        pip_count=$(echo "$pip_issues" | jq 'length' 2>/dev/null || echo 0)
        ((issues_found += pip_count))
        _needle_verbose "pulse: found $pip_count pip vulnerability(ies)"
    fi

    # Run exposed secrets scan
    local secrets_issues
    secrets_issues=$(_pulse_scan_secrets "$workspace")
    if [[ -n "$secrets_issues" ]] && [[ "$secrets_issues" != "[]" ]]; then
        all_issues=$(echo "$all_issues" "$secrets_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        local secrets_count
        secrets_count=$(echo "$secrets_issues" | jq 'length' 2>/dev/null || echo 0)
        ((issues_found += secrets_count))
        _needle_verbose "pulse: found $secrets_count exposed secret(s)"
    fi

    # Run security anti-patterns scan
    local antipattern_issues
    antipattern_issues=$(_pulse_scan_security_antipatterns "$workspace")
    if [[ -n "$antipattern_issues" ]] && [[ "$antipattern_issues" != "[]" ]]; then
        all_issues=$(echo "$all_issues" "$antipattern_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        local antipattern_count
        antipattern_count=$(echo "$antipattern_issues" | jq 'length' 2>/dev/null || echo 0)
        ((issues_found += antipattern_count))
        _needle_verbose "pulse: found $antipattern_count security anti-pattern(s)"
    fi

    # Emit detector completed event
    _needle_telemetry_emit "pulse.detector_completed" "info" \
        "detector=security" \
        "workspace=$workspace" \
        "issues_found=$issues_found"

    _needle_diag_strand "pulse" "Security detector completed" \
        "workspace=$workspace" \
        "issues_found=$issues_found"

    echo "$all_issues"
}

# ============================================================================
# Exposed Secrets Scanner (nd-21h)
# ============================================================================

# Scan source files for exposed secrets (API keys, tokens, passwords, etc.)
#
# Usage: _pulse_scan_secrets <workspace>
# Returns: JSON array of secret issue objects
_pulse_scan_secrets() {
    local workspace="$1"
    local issues="[]"

    # Secret patterns: pattern_name:regex:severity
    # Each entry is "name:severity:pattern"
    local -a secret_patterns=(
        "generic_api_key:high:(?i)(api[_-]?key|apikey)[[:space:]]*[=:][[:space:]]*['\"]?[A-Za-z0-9_\-]{20,}['\"]?"
        "generic_secret:high:(?i)(secret[_-]?key|secret)[[:space:]]*[=:][[:space:]]*['\"]?[A-Za-z0-9_\-]{20,}['\"]?"
        "generic_token:high:(?i)(access[_-]?token|auth[_-]?token|bearer[_-]?token)[[:space:]]*[=:][[:space:]]*['\"]?[A-Za-z0-9_\.\-]{20,}['\"]?"
        "generic_password:high:(?i)(password|passwd|pwd)[[:space:]]*[=:][[:space:]]*['\"]?[^[:space:]'\"]{8,}['\"]?"
        "aws_access_key:critical:(AKIA|ABIA|ACCA|AIPA)[A-Z0-9]{16}"
        "aws_secret:critical:(?i)aws[_-]?secret[_-]?access[_-]?key[[:space:]]*[=:][[:space:]]*['\"]?[A-Za-z0-9/+]{40}['\"]?"
        "github_token:critical:ghp_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{82}"
        "private_key:critical:-----BEGIN (RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----"
        "google_api_key:high:AIza[0-9A-Za-z\-_]{35}"
        "slack_token:high:xox[baprs]-[A-Za-z0-9\-]+"
        "stripe_key:critical:sk_(live|test)_[A-Za-z0-9]{24,}"
        "sendgrid_key:high:SG\.[A-Za-z0-9_\-]{22,}\.[A-Za-z0-9_\-]{43,}"
        "jwt_token:medium:eyJ[A-Za-z0-9_\-]+\.eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+"
        "basic_auth_url:high:https?://[^[:space:]@]+:[^[:space:]@]+@"
    )

    # File extensions to scan
    local scan_extensions="-name '*.sh' -o -name '*.py' -o -name '*.js' -o -name '*.ts' -o -name '*.json' -o -name '*.yaml' -o -name '*.yml' -o -name '*.env' -o -name '*.conf' -o -name '*.config' -o -name '*.rb' -o -name '*.go' -o -name '*.java' -o -name '*.php' -o -name '*.toml' -o -name '*.ini' -o -name '*.xml' -o -name '*.properties'"

    # Collect files to scan
    local scan_files=()
    while IFS= read -r -d '' file; do
        scan_files+=("$file")
    done < <(find "$workspace" -type f \( \
        -name "*.sh" -o -name "*.py" -o -name "*.js" -o -name "*.ts" \
        -o -name "*.json" -o -name "*.yaml" -o -name "*.yml" \
        -o -name "*.env" -o -name "*.conf" -o -name "*.config" \
        -o -name "*.rb" -o -name "*.go" -o -name "*.java" \
        -o -name "*.php" -o -name "*.toml" -o -name "*.ini" \
        -o -name "*.xml" -o -name "*.properties" \
        \) -not -path "*/node_modules/*" -not -path "*/.git/*" \
        -not -path "*/venv/*" -not -path "*/.venv/*" \
        -not -path "*/dist/*" -not -path "*/build/*" \
        -not -path "*/.env.example" \
        -print0 2>/dev/null)

    if [[ ${#scan_files[@]} -eq 0 ]]; then
        echo "[]"
        return 0
    fi

    # Get max issues from config
    local max_secrets
    max_secrets=$(get_config "strands.pulse.max_secrets_per_run" "10")
    local secrets_found=0

    for file in "${scan_files[@]}"; do
        [[ -f "$file" ]] || continue

        # Skip .env.example files (they contain example/placeholder values)
        local basename
        basename=$(basename "$file")
        if [[ "$basename" == ".env.example" ]] || [[ "$basename" == "*.example" ]]; then
            continue
        fi

        local rel_path="${file#$workspace/}"

        # Process each pattern
        for pattern_entry in "${secret_patterns[@]}"; do
            ((secrets_found >= max_secrets)) && break 2

            local pattern_name pattern_severity pattern_regex
            pattern_name="${pattern_entry%%:*}"
            local remainder="${pattern_entry#*:}"
            pattern_severity="${remainder%%:*}"
            pattern_regex="${remainder#*:}"

            # Search for the pattern in the file
            local match_line match_num
            match_line=$(grep -nP "$pattern_regex" "$file" 2>/dev/null | head -1)

            if [[ -z "$match_line" ]]; then
                continue
            fi

            match_num="${match_line%%:*}"
            local match_content="${match_line#*:}"
            # Redact the actual secret value for safety (show only first 4 chars of value)
            match_content=$(echo "$match_content" | sed 's/\(=\s*["'"'"']\?\)[^[:space:]"'"'"']\{4\}\([^[:space:]"'"'"']*\)/\1****REDACTED****/g' 2>/dev/null || echo "${match_content:0:60}...")

            local title="Exposed secret detected: ${pattern_name} in ${rel_path}"
            local fingerprint="secret:${pattern_name}:${rel_path}:${match_num}"

            local description="Potential exposed secret detected in **${rel_path}**.

**Type:** ${pattern_name}
**Severity:** ${pattern_severity}
**Location:** ${rel_path}:${match_num}

\`\`\`
${match_content:0:120}
\`\`\`

## Risk
Exposed secrets in source code can lead to unauthorized access if the code is shared or version-controlled. Even if the secret is rotated, it may remain in git history.

## Remediation
1. **Immediately rotate the exposed credential** - assume it is compromised
2. Remove the secret from source code and git history (use \`git filter-branch\` or \`git-secrets\`)
3. Store secrets in environment variables or a secrets manager
4. Add the file pattern to \`.gitignore\` if it contains secrets
5. Consider using tools like \`git-secrets\` or \`detect-secrets\` pre-commit hooks"

            local issue
            issue=$(jq -n \
                --arg category "security" \
                --arg severity "$pattern_severity" \
                --arg title "$title" \
                --arg description "$description" \
                --arg fingerprint "$fingerprint" \
                --arg labels "secret,exposed-credential,security" \
                '{
                    category: $category,
                    severity: $severity,
                    title: $title,
                    description: $description,
                    fingerprint: $fingerprint,
                    labels: $labels
                }')

            issues=$(echo "$issues" | jq --argjson new_issue "$issue" '. + [$new_issue]' 2>/dev/null || echo "$issues")
            ((secrets_found++))
        done
    done

    echo "$issues"
}

# ============================================================================
# Security Anti-patterns Scanner (nd-21h)
# ============================================================================

# Scan source files for security anti-patterns
#
# Usage: _pulse_scan_security_antipatterns <workspace>
# Returns: JSON array of security anti-pattern issue objects
_pulse_scan_security_antipatterns() {
    local workspace="$1"
    local issues="[]"

    # Anti-pattern definitions: "name:severity:file_types:pattern:description"
    # Using associative-array-like approach for readability
    declare -a antipattern_names antipattern_severities antipattern_extensions antipattern_patterns antipattern_descriptions

    # Shell: dangerous eval usage
    antipattern_names+=("shell_eval")
    antipattern_severities+=("high")
    antipattern_extensions+=("sh bash")
    antipattern_patterns+=('eval\s+.*\$')
    antipattern_descriptions+=("Use of \`eval\` with variable expansion is dangerous and can allow code injection. Consider using safer alternatives like arrays or parameter expansion.")

    # Python: dangerous eval/exec
    antipattern_names+=("python_eval")
    antipattern_severities+=("high")
    antipattern_extensions+=("py")
    antipattern_patterns+=('(eval|exec)\s*\(')
    antipattern_descriptions+=("Use of \`eval()\` or \`exec()\` with untrusted input can allow arbitrary code execution. Avoid these functions or ensure input is strictly validated.")

    # Python: unsafe deserialization (pickle)
    antipattern_names+=("python_pickle")
    antipattern_severities+=("high")
    antipattern_extensions+=("py")
    antipattern_patterns+=('pickle\.(loads?|Unpickler)')
    antipattern_descriptions+=("Use of \`pickle.load()\` on untrusted data is dangerous. Pickle can execute arbitrary code during deserialization. Use safer formats like JSON.")

    # Python: SQL injection via string formatting
    antipattern_names+=("python_sql_injection")
    antipattern_severities+=("critical")
    antipattern_extensions+=("py")
    antipattern_patterns+=('execute\s*\(\s*[f"'"'"'][^"'"'"']*%(s|d)|execute\s*\(\s*"[^"]*"\s*%|execute\s*\(\s*f"')
    antipattern_descriptions+=("Potential SQL injection: query built using string formatting instead of parameterized queries. Use parameterized queries or an ORM.")

    # JavaScript/TypeScript: dangerous eval
    antipattern_names+=("js_eval")
    antipattern_severities+=("high")
    antipattern_extensions+=("js ts jsx tsx")
    antipattern_patterns+=('(^|[^a-zA-Z])(eval|Function)\s*\(')
    antipattern_descriptions+=("Use of \`eval()\` or \`new Function()\` with untrusted input can lead to XSS and code injection. Avoid dynamic code execution.")

    # JavaScript: innerHTML assignment
    antipattern_names+=("js_innerhtml")
    antipattern_severities+=("medium")
    antipattern_extensions+=("js ts jsx tsx html")
    antipattern_patterns+=('\.innerHTML\s*=\s*(?!["'"'"'`])')
    antipattern_descriptions+=("Assigning untrusted data to \`innerHTML\` can lead to XSS. Use \`textContent\`, \`innerText\`, or DOM APIs instead.")

    # JavaScript: document.write
    antipattern_names+=("js_document_write")
    antipattern_severities+=("medium")
    antipattern_extensions+=("js ts jsx tsx html")
    antipattern_patterns+=('document\.write\s*\(')
    antipattern_descriptions+=("Use of \`document.write()\` is a potential XSS vector and is considered a security anti-pattern. Use DOM manipulation methods instead.")

    # Shell: command injection via unquoted variables
    antipattern_names+=("shell_cmd_injection")
    antipattern_severities+=("high")
    antipattern_extensions+=("sh bash")
    antipattern_patterns+=('(system|exec|popen|shell_exec|passthru)\s*\(\s*\$')
    antipattern_descriptions+=("Passing unvalidated variables to shell execution functions can enable command injection. Validate and sanitize all input before executing.")

    # PHP: dangerous functions
    antipattern_names+=("php_dangerous_functions")
    antipattern_severities+=("high")
    antipattern_extensions+=("php")
    antipattern_patterns+=('(system|exec|popen|shell_exec|passthru|eval)\s*\(\s*\$')
    antipattern_descriptions+=("PHP function with direct variable argument can enable command injection or code execution. Validate and sanitize all input.")

    # Python: subprocess with shell=True
    antipattern_names+=("python_subprocess_shell")
    antipattern_severities+=("medium")
    antipattern_extensions+=("py")
    antipattern_patterns+=('subprocess\.(run|call|Popen|check_output|check_call)\s*\([^)]*shell\s*=\s*True')
    antipattern_descriptions+=("Using \`subprocess\` with \`shell=True\` can enable command injection if any part of the command is untrusted. Use a list of arguments instead.")

    # Python: os.system
    antipattern_names+=("python_os_system")
    antipattern_severities+=("medium")
    antipattern_extensions+=("py")
    antipattern_patterns+=('os\.(system|popen)\s*\(')
    antipattern_descriptions+=("Use of \`os.system()\` or \`os.popen()\` with untrusted input can enable command injection. Prefer \`subprocess\` with a list of arguments.")

    # Any: hardcoded localhost/IP in production code
    antipattern_names+=("hardcoded_url")
    antipattern_severities+=("low")
    antipattern_extensions+=("py js ts go java rb php")
    antipattern_patterns+=('(http://localhost|http://127\.0\.0\.1|http://0\.0\.0\.0)')
    antipattern_descriptions+=("Hardcoded localhost URLs will not work in production. Use configuration/environment variables for service URLs.")

    # Get max issues from config
    local max_antipatterns
    max_antipatterns=$(get_config "strands.pulse.max_antipatterns_per_run" "10")
    local antipatterns_found=0

    local num_patterns=${#antipattern_names[@]}

    for ((i = 0; i < num_patterns; i++)); do
        ((antipatterns_found >= max_antipatterns)) && break

        local pname="${antipattern_names[$i]}"
        local pseverity="${antipattern_severities[$i]}"
        local pextensions="${antipattern_extensions[$i]}"
        local ppattern="${antipattern_patterns[$i]}"
        local pdesc="${antipattern_descriptions[$i]}"

        # Build find arguments for the file extensions
        local find_args=()
        local first_ext=true
        for ext in $pextensions; do
            if [[ "$first_ext" == "true" ]]; then
                find_args+=("-name" "*.${ext}")
                first_ext=false
            else
                find_args+=("-o" "-name" "*.${ext}")
            fi
        done

        # Find matching files
        local match_files=()
        while IFS= read -r -d '' file; do
            match_files+=("$file")
        done < <(find "$workspace" -type f \( "${find_args[@]}" \) \
            -not -path "*/node_modules/*" -not -path "*/.git/*" \
            -not -path "*/venv/*" -not -path "*/.venv/*" \
            -not -path "*/dist/*" -not -path "*/build/*" \
            -print0 2>/dev/null)

        for file in "${match_files[@]}"; do
            ((antipatterns_found >= max_antipatterns)) && break 2

            [[ -f "$file" ]] || continue

            # Skip test files (anti-patterns may be intentional in tests)
            local rel_path="${file#$workspace/}"
            if [[ "$rel_path" == *"test"* ]] || [[ "$rel_path" == *"spec"* ]] || [[ "$rel_path" == *"__tests__"* ]]; then
                continue
            fi

            # Search for the anti-pattern
            local match_line match_num
            match_line=$(grep -nP "$ppattern" "$file" 2>/dev/null | head -1)

            if [[ -z "$match_line" ]]; then
                continue
            fi

            match_num="${match_line%%:*}"
            local match_content="${match_line#*:}"
            match_content="${match_content:0:120}"

            local title="Security anti-pattern: ${pname} in ${rel_path}"
            local fingerprint="antipattern:${pname}:${rel_path}:${match_num}"

            local description="Security anti-pattern detected in **${rel_path}**.

**Pattern:** ${pname}
**Severity:** ${pseverity}
**Location:** ${rel_path}:${match_num}

\`\`\`
${match_content}
\`\`\`

## Risk
${pdesc}

## Remediation
Review the flagged code and replace with a secure alternative. Refer to OWASP guidelines for secure coding practices."

            local issue
            issue=$(jq -n \
                --arg category "security" \
                --arg severity "$pseverity" \
                --arg title "$title" \
                --arg description "$description" \
                --arg fingerprint "$fingerprint" \
                --arg labels "security,anti-pattern,${pname}" \
                '{
                    category: $category,
                    severity: $severity,
                    title: $title,
                    description: $description,
                    fingerprint: $fingerprint,
                    labels: $labels
                }')

            issues=$(echo "$issues" | jq --argjson new_issue "$issue" '. + [$new_issue]' 2>/dev/null || echo "$issues")
            ((antipatterns_found++))
        done
    done

    echo "$issues"
}

# ============================================================================
# Dependency Freshness Detector (nd-1fr)
# ============================================================================

# Get package last publish date from npm registry
# Usage: _pulse_get_npm_package_age <package_name>
# Returns: Age in days, or -1 if unavailable
_pulse_get_npm_package_age() {
    local pkg_name="$1"

    # Default to -1 (unknown) if we can't fetch
    local age_days=-1

    # Query npm registry API
    # Endpoint: https://registry.npmjs.org/-/package/<package>/dist-tags
    local api_url="https://registry.npmjs.org/-/package/${pkg_name}/dist-tags"

    # Use curl with timeout and silent mode
    local response
    response=$(curl -sSf --connect-timeout 5 --max-time 10 "$api_url" 2>/dev/null) || {
        _needle_debug "pulse: failed to fetch npm package info for $pkg_name"
        echo -1
        return 0
    }

    # Get the latest version
    local latest_version
    latest_version=$(echo "$response" | jq -r '.latest // empty' 2>/dev/null)

    if [[ -z "$latest_version" ]]; then
        echo -1
        return 0
    fi

    # Now fetch the package metadata to get the last publish date
    local meta_url="https://registry.npmjs.org/${pkg_name}"
    local meta_response
    meta_response=$(curl -sSf --connect-timeout 5 --max-time 10 "$meta_url" 2>/dev/null) || {
        echo -1
        return 0
    }

    # Get the time object for the latest version
    local last_modified
    last_modified=$(echo "$meta_response" | jq -r '.time["'"$latest_version"'"] // empty' 2>/dev/null)

    if [[ -z "$last_modified" ]]; then
        echo -1
        return 0
    fi

    # Calculate age in days
    # Convert ISO timestamp to epoch, then calculate days
    local modified_epoch now_epoch
    modified_epoch=$(date -d "$last_modified" +%s 2>/dev/null || echo 0)

    if [[ "$modified_epoch" -eq 0 ]]; then
        echo -1
        return 0
    fi

    now_epoch=$(date +%s)
    local age_seconds=$((now_epoch - modified_epoch))
    age_days=$((age_seconds / 86400))

    echo "$age_days"
}

# Get package last publish date from PyPI
# Usage: _pulse_get_pip_package_age <package_name>
# Returns: Age in days, or -1 if unavailable
_pulse_get_pip_package_age() {
    local pkg_name="$1"

    # Default to -1 (unknown) if we can't fetch
    local age_days=-1

    # Query PyPI JSON API
    # Endpoint: https://pypi.org/pypi/<package>/json
    local api_url="https://pypi.org/pypi/${pkg_name}/json"

    # Use curl with timeout and silent mode
    local response
    response=$(curl -sSf --connect-timeout 5 --max-time 10 "$api_url" 2>/dev/null) || {
        _needle_debug "pulse: failed to fetch PyPI package info for $pkg_name"
        echo -1
        return 0
    }

    # Get the last release upload time
    # PyPI returns: { "urls": [...], "info": {...}, "last_serial": ... }
    # We want the most recent upload time from urls or info.version
    local last_uploaded
    last_uploaded=$(echo "$response" | jq -r '.urls[-1].upload_time // .info.release_url // empty' 2>/dev/null)

    # Alternative: get the latest version's upload time
    if [[ -z "$last_uploaded" ]]; then
        # Try to get the upload_time_iso from the most recent release
        last_uploaded=$(echo "$response" | jq -r '.urls | sort_by(.upload_time) | .[-1].upload_time // empty' 2>/dev/null)
    fi

    # Another approach: get from releases
    if [[ -z "$last_uploaded" ]]; then
        local latest_version
        latest_version=$(echo "$response" | jq -r '.info.version // empty' 2>/dev/null)
        if [[ -n "$latest_version" ]]; then
            last_uploaded=$(echo "$response" | jq -r '.releases["'"$latest_version"'"][-1].upload_time // empty' 2>/dev/null)
        fi
    fi

    if [[ -z "$last_uploaded" ]]; then
        echo -1
        return 0
    fi

    # Calculate age in days
    # Handle both ISO format and Unix timestamp
    local modified_epoch now_epoch
    if [[ "$last_uploaded" =~ ^[0-9]+$ ]]; then
        # Unix timestamp
        modified_epoch="$last_uploaded"
    else
        # ISO format
        modified_epoch=$(date -d "$last_uploaded" +%s 2>/dev/null || echo 0)
    fi

    if [[ "$modified_epoch" -eq 0 ]]; then
        echo -1
        return 0
    fi

    now_epoch=$(date +%s)
    local age_seconds=$((now_epoch - modified_epoch))
    age_days=$((age_seconds / 86400))

    echo "$age_days"
}

# Get installed version from package.json
# Usage: _pulse_get_npm_installed_version <workspace> <package_name>
# Returns: Installed version or empty string
_pulse_get_npm_installed_version() {
    local workspace="$1"
    local pkg_name="$2"

    if [[ ! -f "$workspace/package.json" ]]; then
        echo ""
        return 0
    fi

    # Check both dependencies and devDependencies
    local version
    version=$(jq -r '.dependencies["'"$pkg_name"'"] // .devDependencies["'"$pkg_name"'"] // empty' "$workspace/package.json" 2>/dev/null)

    # Clean version (remove ^ or ~ prefix)
    version="${version#^}"
    version="${version#\~}"

    echo "$version"
}

# Get installed version from requirements.txt
# Usage: _pulse_get_pip_installed_version <workspace> <package_name>
# Returns: Installed version or empty string
_pulse_get_pip_installed_version() {
    local workspace="$1"
    local pkg_name="$2"

    # Check requirements.txt
    if [[ -f "$workspace/requirements.txt" ]]; then
        # Match patterns like: package==1.2.3, package>=1.2.3, package~=1.2.3
        local version
        version=$(grep -iE "^${pkg_name}[=<>~]+" "$workspace/requirements.txt" 2>/dev/null | head -1 | sed -E 's/.*[=<>~]+//' | cut -d' ' -f1)
        # Clean version (remove trailing whitespace or comments)
        version="${version%%#*}"
        version="${version%% *}"
        echo "$version"
        return 0
    fi

    # Check pyproject.toml for Poetry projects
    if [[ -f "$workspace/pyproject.toml" ]]; then
        local version
        version=$(grep -A2 -iE "^\[tool\.poetry\.dependencies\]" "$workspace/pyproject.toml" 2>/dev/null | \
                  grep -iE "^${pkg_name}[[:space:]]*=" | \
                  sed -E 's/.*=[[:space:]]*"([^"]+)".*/\1/' 2>/dev/null || echo "")
        if [[ -n "$version" ]]; then
            echo "$version"
            return 0
        fi
    fi

    echo ""
}

# Get latest version from npm registry
# Usage: _pulse_get_npm_latest_version <package_name>
# Returns: Latest version or empty string
_pulse_get_npm_latest_version() {
    local pkg_name="$1"

    local api_url="https://registry.npmjs.org/-/package/${pkg_name}/dist-tags"

    local response
    response=$(curl -sSf --connect-timeout 5 --max-time 10 "$api_url" 2>/dev/null) || {
        echo ""
        return 0
    }

    jq -r '.latest // empty' <<< "$response" 2>/dev/null || echo ""
}

# Get latest version from PyPI
# Usage: _pulse_get_pip_latest_version <package_name>
# Returns: Latest version or empty string
_pulse_get_pip_latest_version() {
    local pkg_name="$1"

    local api_url="https://pypi.org/pypi/${pkg_name}/json"

    local response
    response=$(curl -sSf --connect-timeout 5 --max-time 10 "$api_url" 2>/dev/null) || {
        echo ""
        return 0
    }

    jq -r '.info.version // empty' <<< "$response" 2>/dev/null || echo ""
}

# Scan Node.js dependencies for staleness
# Usage: _pulse_scan_npm_deps <workspace>
# Returns: JSON array of stale dependency issue objects
_pulse_scan_npm_deps() {
    local workspace="$1"
    local issues="[]"

    # Check for package.json
    if [[ ! -f "$workspace/package.json" ]]; then
        echo "[]"
        return 0
    fi

    # Get threshold from config (default: 365 days)
    local threshold
    threshold=$(get_config "strands.pulse.stale_threshold_days" "365")

    # Get max deps to check per run
    local max_deps
    max_deps=$(get_config "strands.pulse.max_deps_per_run" "10")

    # Get dependencies list (both dependencies and devDependencies)
    local deps
    deps=$(jq -r '(.dependencies // {}) + (.devDependencies // {}) | keys[]' "$workspace/package.json" 2>/dev/null)

    if [[ -z "$deps" ]]; then
        echo "[]"
        return 0
    fi

    local deps_checked=0

    while IFS= read -r pkg_name && ((deps_checked < max_deps)); do
        [[ -z "$pkg_name" ]] && continue

        # Skip private/scoped packages that might not be on npm
        if [[ "$pkg_name" == @* ]]; then
            continue
        fi

        # Get package age
        local age_days
        age_days=$(_pulse_get_npm_package_age "$pkg_name")

        # Skip if we couldn't determine age
        if [[ "$age_days" -lt 0 ]]; then
            _needle_debug "pulse: could not determine age for npm package: $pkg_name"
            continue
        fi

        ((deps_checked++))

        # Check if stale
        if ((age_days >= threshold)); then
            local installed_version latest_version
            installed_version=$(_pulse_get_npm_installed_version "$workspace" "$pkg_name")
            latest_version=$(_pulse_get_npm_latest_version "$pkg_name")

            local title="Update stale npm dependency: ${pkg_name} (${age_days} days old)"
            local fingerprint="stale-dep:npm:${pkg_name}"

            local description="npm package **${pkg_name}** has not been updated in ${age_days} days.

**Current Version:** ${installed_version:-unknown}
**Latest Version:** ${latest_version:-unknown}
**Last Updated:** ${age_days} days ago
**Threshold:** ${threshold} days

## Context
This dependency hasn't received updates in over a year, which may indicate:
- The package is abandoned or unmaintained
- Security vulnerabilities may exist without patches
- Compatibility issues with newer runtimes/libraries

## Remediation
1. Check if a newer version is available: \`npm outdated ${pkg_name}\`
2. Review the package's changelog for breaking changes
3. Update the dependency: \`npm update ${pkg_name}\` or \`npm install ${pkg_name}@latest\`
4. Test thoroughly after updating
5. Consider alternatives if the package is abandoned"

            # Determine severity based on age
            local severity="low"
            if ((age_days >= 730)); then
                severity="medium"  # 2+ years
            fi
            if ((age_days >= 1095)); then
                severity="high"    # 3+ years
            fi

            local issue
            issue=$(jq -n \
                --arg category "dependencies" \
                --arg severity "$severity" \
                --arg title "$title" \
                --arg description "$description" \
                --arg fingerprint "$fingerprint" \
                --arg labels "npm,dependencies,stale,maintenance" \
                '{
                    category: $category,
                    severity: $severity,
                    title: $title,
                    description: $description,
                    fingerprint: $fingerprint,
                    labels: $labels
                }')

            issues=$(echo "$issues" "$issue" | jq -s 'add' 2>/dev/null || echo "$issues")
        fi
    done <<< "$deps"

    echo "$issues"
}

# Scan Python dependencies for staleness
# Usage: _pulse_scan_pip_deps <workspace>
# Returns: JSON array of stale dependency issue objects
_pulse_scan_pip_deps() {
    local workspace="$1"
    local issues="[]"

    # Check for Python project files
    local has_python_reqs=false
    if [[ -f "$workspace/requirements.txt" ]] || [[ -f "$workspace/pyproject.toml" ]] || \
       [[ -f "$workspace/setup.py" ]] || [[ -f "$workspace/Pipfile" ]]; then
        has_python_reqs=true
    fi

    if [[ "$has_python_reqs" != "true" ]]; then
        echo "[]"
        return 0
    fi

    # Get threshold from config (default: 365 days)
    local threshold
    threshold=$(get_config "strands.pulse.stale_threshold_days" "365")

    # Get max deps to check per run
    local max_deps
    max_deps=$(get_config "strands.pulse.max_deps_per_run" "10")

    # Parse requirements.txt for package names
    local pkgs=()
    if [[ -f "$workspace/requirements.txt" ]]; then
        while IFS= read -r line; do
            # Skip comments, empty lines, and pip options
            [[ "$line" =~ ^[[:space:]]*# ]] && continue
            [[ -z "${line// }" ]] && continue
            [[ "$line" =~ ^-[a-z] ]] && continue
            [[ "$line" =~ ^-- ]] && continue

            # Extract package name (before version specifier)
            local pkg_name
            pkg_name=$(echo "$line" | sed -E 's/([a-zA-Z0-9_-]+).*/\1/' | head -1)
            pkg_name="${pkg_name%%\[*}"  # Remove extras marker

            if [[ -n "$pkg_name" ]]; then
                pkgs+=("$pkg_name")
            fi
        done < "$workspace/requirements.txt"
    fi

    if [[ ${#pkgs[@]} -eq 0 ]]; then
        echo "[]"
        return 0
    fi

    local deps_checked=0

    for pkg_name in "${pkgs[@]}"; do
        ((deps_checked >= max_deps)) && break

        # Get package age
        local age_days
        age_days=$(_pulse_get_pip_package_age "$pkg_name")

        # Skip if we couldn't determine age
        if [[ "$age_days" -lt 0 ]]; then
            _needle_debug "pulse: could not determine age for pip package: $pkg_name"
            continue
        fi

        ((deps_checked++))

        # Check if stale
        if ((age_days >= threshold)); then
            local installed_version latest_version
            installed_version=$(_pulse_get_pip_installed_version "$workspace" "$pkg_name")
            latest_version=$(_pulse_get_pip_latest_version "$pkg_name")

            local title="Update stale pip dependency: ${pkg_name} (${age_days} days old)"
            local fingerprint="stale-dep:pip:${pkg_name}"

            local description="Python package **${pkg_name}** has not been updated in ${age_days} days.

**Current Version:** ${installed_version:-unknown}
**Latest Version:** ${latest_version:-unknown}
**Last Updated:** ${age_days} days ago
**Threshold:** ${threshold} days

## Context
This dependency hasn't received updates in over a year, which may indicate:
- The package is abandoned or unmaintained
- Security vulnerabilities may exist without patches
- Compatibility issues with newer Python versions

## Remediation
1. Check for updates: \`pip index versions ${pkg_name}\`
2. Review the package's changelog for breaking changes
3. Update the dependency: \`pip install --upgrade ${pkg_name}\`
4. Update requirements.txt with the new version
5. Test thoroughly after updating
6. Consider alternatives if the package is abandoned"

            # Determine severity based on age
            local severity="low"
            if ((age_days >= 730)); then
                severity="medium"  # 2+ years
            fi
            if ((age_days >= 1095)); then
                severity="high"    # 3+ years
            fi

            local issue
            issue=$(jq -n \
                --arg category "dependencies" \
                --arg severity "$severity" \
                --arg title "$title" \
                --arg description "$description" \
                --arg fingerprint "$fingerprint" \
                --arg labels "pip,python,dependencies,stale,maintenance" \
                '{
                    category: $category,
                    severity: $severity,
                    title: $title,
                    description: $description,
                    fingerprint: $fingerprint,
                    labels: $labels
                }')

            issues=$(echo "$issues" "$issue" | jq -s 'add' 2>/dev/null || echo "$issues")
        fi
    done

    echo "$issues"
}

# Main dependency freshness detector
# Scans for stale/outdated dependencies in Node.js and Python projects
#
# Usage: _pulse_detector_dependencies <workspace> <agent>
# Returns: JSON array of dependency issue objects
_pulse_detector_dependencies() {
    local workspace="$1"
    local agent="$2"

    # Check if dependencies detector is enabled
    local deps_enabled
    deps_enabled=$(get_config "strands.pulse.detectors.dependencies" "true")

    if [[ "$deps_enabled" != "true" ]]; then
        echo "[]"
        return 0
    fi

    _needle_diag_strand "pulse" "Running dependency freshness detector" \
        "workspace=$workspace" \
        "agent=$agent"

    # Emit detector started event
    _needle_telemetry_emit "pulse.detector_started" "info" \
        "detector=dependencies" \
        "workspace=$workspace"

    local all_issues="[]"
    local issues_found=0

    # Scan npm dependencies for Node.js projects
    local npm_issues
    npm_issues=$(_pulse_scan_npm_deps "$workspace")
    if [[ -n "$npm_issues" ]] && [[ "$npm_issues" != "[]" ]]; then
        all_issues=$(echo "$all_issues" "$npm_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        local npm_count
        npm_count=$(echo "$npm_issues" | jq 'length' 2>/dev/null || echo 0)
        ((issues_found += npm_count))
        _needle_verbose "pulse: found $npm_count stale npm dependency(ies)"
    fi

    # Scan pip dependencies for Python projects
    local pip_issues
    pip_issues=$(_pulse_scan_pip_deps "$workspace")
    if [[ -n "$pip_issues" ]] && [[ "$pip_issues" != "[]" ]]; then
        all_issues=$(echo "$all_issues" "$pip_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        local pip_count
        pip_count=$(echo "$pip_issues" | jq 'length' 2>/dev/null || echo 0)
        ((issues_found += pip_count))
        _needle_verbose "pulse: found $pip_count stale pip dependency(ies)"
    fi

    # Emit detector completed event
    _needle_telemetry_emit "pulse.detector_completed" "info" \
        "detector=dependencies" \
        "workspace=$workspace" \
        "issues_found=$issues_found"

    _needle_diag_strand "pulse" "Dependency freshness detector completed" \
        "workspace=$workspace" \
        "issues_found=$issues_found"

    echo "$all_issues"
}

# ============================================================================
# Documentation Drift Detector (nd-gn2)
# ============================================================================

# Extract code references from markdown content
# Looks for common patterns like:
# - `function_name()`
# - file paths: src/path/to/file.ext
# - import statements
# - API endpoints
#
# Usage: _pulse_extract_doc_refs <markdown_content>
# Returns: JSON array of reference objects
_pulse_extract_doc_refs() {
    local content="$1"
    local refs="[]"

    # Pattern 1: Backticked function/method names with parens: `functionName()`
    # Use $'\x60' to represent backtick character to avoid shell parsing issues
    local bt=$'\x60'
    while IFS= read -r match; do
        [[ -z "$match" ]] && continue
        # Extract function name from match like `calculate_cost()`
        local func_name="$match"
        func_name="${func_name#*${bt}}"     # Remove up to and including leading backtick
        func_name="${func_name%${bt}*}"     # Remove trailing backtick and everything after
        func_name="${func_name%()}"         # Remove trailing ()
        func_name="${func_name%(}"          # Remove trailing ( if present

        if [[ -n "$func_name" ]] && [[ ${#func_name} -ge 2 ]]; then
            local ref
            ref=$(jq -n --arg type "function" --arg name "$func_name" \
                '{type: $type, name: $name}')
            refs=$(echo "$refs" "$ref" | jq -s 'add' 2>/dev/null || echo "$refs")
        fi
    done < <(echo "$content" | grep -oE "${bt}[a-zA-Z_][a-zA-Z0-9_]*\(\)${bt}" 2>/dev/null)

    # Pattern 2: File path references: src/path/to/file.ext or ./path/to/file.ext
    while IFS= read -r match; do
        [[ -z "$match" ]] && continue
        # Clean up the match - extract just the path
        local file_path="$match"
        file_path="${file_path#*${bt}}"    # Remove up to and including leading backtick
        file_path="${file_path%${bt}*}"    # Remove trailing backtick and everything after
        file_path="${file_path#(}"         # Remove leading paren
        file_path="${file_path%)}"         # Remove trailing paren

        # Validate it looks like a file path
        if [[ -n "$file_path" ]] && [[ "$file_path" =~ ^(\./|/|[a-zA-Z]) ]] && [[ "$file_path" =~ \.(sh|js|ts|py|go|rs|java|rb|php|yaml|yml|json|md)$ ]]; then
            local ref
            ref=$(jq -n --arg type "file" --arg path "$file_path" \
                '{type: $type, path: $path}')
            refs=$(echo "$refs" "$ref" | jq -s 'add' 2>/dev/null || echo "$refs")
        fi
    done < <(echo "$content" | grep -oE "[${bt}()]?[./]?[a-zA-Z0-9_/-]+[.][a-zA-Z]{2,4}[${bt}:)]?" 2>/dev/null | head -50)

    echo "$refs"
}

# Check if a function exists in the codebase
# Usage: _pulse_function_exists <workspace> <function_name>
# Returns: 0 if found, 1 if not found
_pulse_function_exists() {
    local workspace="$1"
    local func_name="$2"

    # Search for function definition patterns
    # Bash: func_name() { or function func_name {
    # Python: def func_name(
    # JavaScript: function func_name( or func_name = ( or func_name(
    # TypeScript: same as JS plus func_name(

    local patterns=(
        "^[[:space:]]*(function[[:space:]]+)?${func_name}[[:space:]]*\(\)"
        "^[[:space:]]*def[[:space:]]+${func_name}[[:space:]]*\("
        "^[[:space:]]*${func_name}[[:space:]]*=[[:space:]]*(async[[:space:]]+)?\("
        "^[[:space:]]*(const|let|var)[[:space:]]+${func_name}[[:space:]]*="
    )

    for pattern in "${patterns[@]}"; do
        if grep -rqE "$pattern" "$workspace" --include="*.sh" --include="*.py" --include="*.js" --include="*.ts" 2>/dev/null; then
            return 0
        fi
    done

    return 1
}

# Scan documentation files for references
# Usage: _pulse_scan_docs <workspace>
# Returns: JSON array of doc issue objects
_pulse_scan_docs() {
    local workspace="$1"
    local issues="[]"

    # Find documentation files
    local doc_files=()
    while IFS= read -r -d '' file; do
        doc_files+=("$file")
    done < <(find "$workspace" -type f \( -name "*.md" -o -name "README*" -o -name "*.rst" \) \
        -not -path "*/node_modules/*" -not -path "*/.git/*" -not -path "*/venv/*" -print0 2>/dev/null | head -20)

    if [[ ${#doc_files[@]} -eq 0 ]]; then
        echo "[]"
        return 0
    fi

    local doc_drift_enabled
    doc_drift_enabled=$(get_config "strands.pulse.detectors.doc_drift_enabled" "true")

    if [[ "$doc_drift_enabled" != "true" ]]; then
        echo "[]"
        return 0
    fi

    for doc_file in "${doc_files[@]}"; do
        local rel_path="${doc_file#$workspace/}"
        local content
        content=$(cat "$doc_file" 2>/dev/null) || continue

        # Extract references
        local refs
        refs=$(_pulse_extract_doc_refs "$content")

        local ref_count
        ref_count=$(echo "$refs" | jq 'length' 2>/dev/null || echo 0)

        if [[ "$ref_count" -eq 0 ]]; then
            continue
        fi

        # Check each reference
        local idx=0
        while ((idx < ref_count)); do
            local ref ref_type

            ref=$(echo "$refs" | jq -c ".[$idx]" 2>/dev/null)
            ref_type=$(echo "$ref" | jq -r '.type' 2>/dev/null)

            local broken_ref=""

            case "$ref_type" in
                function)
                    local func_name
                    func_name=$(echo "$ref" | jq -r '.name' 2>/dev/null)

                    if [[ -n "$func_name" ]] && ! _pulse_function_exists "$workspace" "$func_name"; then
                        broken_ref="function \`$func_name()\`"
                    fi
                    ;;
                file)
                    local file_path
                    file_path=$(echo "$ref" | jq -r '.path' 2>/dev/null)

                    # Try multiple path resolutions
                    local resolved_path=""
                    if [[ -f "$workspace/$file_path" ]]; then
                        resolved_path="$workspace/$file_path"
                    elif [[ -f "$workspace/${rel_path%/*}/$file_path" ]]; then
                        resolved_path="$workspace/${rel_path%/*}/$file_path"
                    fi

                    if [[ -z "$resolved_path" ]]; then
                        broken_ref="file \`$file_path\`"
                    fi
                    ;;
            esac

            if [[ -n "$broken_ref" ]]; then
                local title="Documentation drift: $broken_ref in $rel_path"
                local fingerprint="doc-drift:${rel_path}:${broken_ref}"

                local description="Documentation reference drift detected in **${rel_path}**.

**Missing Reference:** ${broken_ref}

## Context
The documentation references code that no longer exists or has been moved. This creates confusion for users and developers.

## Remediation
1. Search the codebase for the renamed/moved code
2. Update the documentation to reference the current location
3. If the code was removed, remove or update the documentation"

                local issue
                issue=$(jq -n \
                    --arg category "docs" \
                    --arg severity "low" \
                    --arg title "$title" \
                    --arg description "$description" \
                    --arg fingerprint "$fingerprint" \
                    --arg labels "documentation,drift" \
                    '{
                        category: $category,
                        severity: $severity,
                        title: $title,
                        description: $description,
                        fingerprint: $fingerprint,
                        labels: $labels
                    }')

                issues=$(echo "$issues" "$issue" | jq -s 'add' 2>/dev/null || echo "$issues")
            fi

            ((idx++))
        done
    done

    echo "$issues"
}

# Main documentation drift detector
# Usage: _pulse_detector_docs <workspace> <agent>
# Returns: JSON array of doc issue objects
_pulse_detector_docs() {
    local workspace="$1"
    local agent="$2"

    # Check if docs detector is enabled
    local docs_enabled
    docs_enabled=$(get_config "strands.pulse.detectors.docs" "true")

    if [[ "$docs_enabled" != "true" ]]; then
        echo "[]"
        return 0
    fi

    _needle_diag_strand "pulse" "Running documentation drift detector" \
        "workspace=$workspace" \
        "agent=$agent"

    _needle_telemetry_emit "pulse.detector_started" "info" \
        "detector=docs" \
        "workspace=$workspace"

    local issues
    issues=$(_pulse_scan_docs "$workspace")

    local issue_count
    issue_count=$(echo "$issues" | jq 'length' 2>/dev/null || echo 0)

    _needle_telemetry_emit "pulse.detector_completed" "info" \
        "detector=docs" \
        "workspace=$workspace" \
        "issues_found=$issue_count"

    _needle_diag_strand "pulse" "Documentation drift detector completed" \
        "workspace=$workspace" \
        "issues_found=$issue_count"

    echo "$issues"
}

# ============================================================================
# Test Coverage Gap Detector (nd-gn2)
# ============================================================================

# Run npm/jest coverage and parse results
# Usage: _pulse_npm_coverage <workspace>
# Returns: JSON array of coverage issue objects
_pulse_npm_coverage() {
    local workspace="$1"
    local issues="[]"

    # Check for package.json and coverage script
    if [[ ! -f "$workspace/package.json" ]]; then
        echo "[]"
        return 0
    fi

    # Check if npm is available
    if ! command -v npm &>/dev/null; then
        _needle_debug "pulse: npm not found, skipping JS coverage scan"
        echo "[]"
        return 0
    fi

    # Check for coverage script or common coverage tools
    local has_coverage=false
    if grep -q '"coverage"' "$workspace/package.json" 2>/dev/null; then
        has_coverage=true
    elif [[ -f "$workspace/jest.config.js" ]] || [[ -f "$workspace/jest.config.ts" ]]; then
        has_coverage=true
    elif [[ -f "$workspace/.nycrc" ]] || [[ -f "$workspace/nyc.config.js" ]]; then
        has_coverage=true
    fi

    if [[ "$has_coverage" != "true" ]]; then
        echo "[]"
        return 0
    fi

    # Run coverage (this may take a while)
    local coverage_output
    coverage_output=$(cd "$workspace" && npm run coverage -- --json --outputFile=/tmp/coverage-report.json 2>/dev/null) || {
        # Try alternative coverage commands
        coverage_output=$(cd "$workspace" && npx nyc report --reporter=json 2>/dev/null) || {
            echo "[]"
            return 0
        }
    }

    # Parse coverage summary
    local coverage_file="$workspace/coverage/coverage-final.json"
    if [[ ! -f "$coverage_file" ]]; then
        coverage_file="/tmp/coverage-report.json"
    fi

    if [[ ! -f "$coverage_file" ]]; then
        echo "[]"
        return 0
    fi

    # Get coverage threshold from config
    local threshold
    threshold=$(get_config "strands.pulse.coverage_threshold" "70")

    # Parse coverage data
    # Istanbul/nyc format: { "filePath": "...", "coverage": {...} }
    local file_idx=0
    local file_count
    file_count=$(jq 'length' "$coverage_file" 2>/dev/null || echo 0)

    while ((file_idx < file_count)); do
        local file_entry file_path line_coverage
        file_entry=$(jq -c ".[$file_idx]" "$coverage_file" 2>/dev/null)
        file_path=$(echo "$file_entry" | jq -r '.path // .filePath // empty' 2>/dev/null)

        [[ -z "$file_path" ]] && { ((file_idx++)); continue; }

        # Calculate coverage percentage
        local covered total pct
        covered=$(echo "$file_entry" | jq '[.l | to_entries[] | .value | select(. > 0)] | length // 0' 2>/dev/null || echo 0)
        total=$(echo "$file_entry" | jq '[.l | to_entries[]] | length // 1' 2>/dev/null || echo 1)

        if [[ "$total" -gt 0 ]]; then
            pct=$((covered * 100 / total))
        else
            pct=100
        fi

        if ((pct < threshold)); then
            local rel_path="${file_path#$workspace/}"
            local title="Low test coverage: $rel_path (${pct}%)"
            local fingerprint="coverage:${rel_path}"

            local description="Test coverage below threshold detected for **${rel_path}**.

**Coverage:** ${pct}%
**Threshold:** ${threshold}%
**Lines Covered:** ${covered}/${total}

## Context
Low test coverage increases the risk of bugs going undetected. This file needs more comprehensive tests.

## Remediation
1. Identify untested code paths in the file
2. Write unit tests for critical functions
3. Focus on edge cases and error handling
4. Aim for at least ${threshold}% coverage"

            local issue
            issue=$(jq -n \
                --arg category "coverage" \
                --arg severity "medium" \
                --arg title "$title" \
                --arg description "$description" \
                --arg fingerprint "$fingerprint" \
                --arg labels "testing,coverage,javascript" \
                '{
                    category: $category,
                    severity: $severity,
                    title: $title,
                    description: $description,
                    fingerprint: $fingerprint,
                    labels: $labels
                }')

            issues=$(echo "$issues" "$issue" | jq -s 'add' 2>/dev/null || echo "$issues")
        fi

        ((file_idx++))
    done

    echo "$issues"
}

# Run pytest coverage and parse results
# Usage: _pulse_pytest_coverage <workspace>
# Returns: JSON array of coverage issue objects
_pulse_pytest_coverage() {
    local workspace="$1"
    local issues="[]"

    # Check for Python project files
    local has_python=false
    if [[ -f "$workspace/pyproject.toml" ]] || [[ -f "$workspace/setup.py" ]] || \
       [[ -f "$workspace/pytest.ini" ]] || [[ -f "$workspace/tox.ini" ]]; then
        has_python=true
    fi

    if [[ "$has_python" != "true" ]]; then
        echo "[]"
        return 0
    fi

    # Check if pytest and pytest-cov are available
    if ! command -v pytest &>/dev/null; then
        _needle_debug "pulse: pytest not found, skipping Python coverage scan"
        echo "[]"
        return 0
    fi

    # Get coverage threshold from config
    local threshold
    threshold=$(get_config "strands.pulse.coverage_threshold" "70")

    # Run pytest with coverage
    local coverage_output
    coverage_output=$(cd "$workspace" && pytest --cov=. --cov-report=json:/tmp/pytest-coverage.json --collect-only -q 2>/dev/null) || {
        echo "[]"
        return 0
    }

    local coverage_file="/tmp/pytest-coverage.json"
    if [[ ! -f "$coverage_file" ]]; then
        echo "[]"
        return 0
    fi

    # Parse coverage JSON
    # pytest-cov format: { "files": [{ "file": "...", "summary": { "percent_covered": 45.5 } }] }
    local file_count
    file_count=$(jq '.files | length // 0' "$coverage_file" 2>/dev/null || echo 0)

    local idx=0
    while ((idx < file_count)); do
        local file_entry file_path pct
        file_entry=$(jq -c ".files[$idx]" "$coverage_file" 2>/dev/null)
        file_path=$(echo "$file_entry" | jq -r '.file // empty' 2>/dev/null)
        pct=$(echo "$file_entry" | jq -r '.summary.percent_covered // 100' 2>/dev/null)

        [[ -z "$file_path" ]] && { ((idx++)); continue; }

        # Skip __pycache__ and test files
        if [[ "$file_path" == *"__pycache__"* ]] || [[ "$file_path" == *"test_"* ]] || [[ "$file_path" == *"_test.py" ]]; then
            ((idx++))
            continue
        fi

        # Convert to integer for comparison
        local pct_int
        pct_int=$(echo "$pct" | awk '{printf "%.0f", $1}')

        if ((pct_int < threshold)); then
            local rel_path="$file_path"
            local title="Low test coverage: $rel_path (${pct_int}%)"
            local fingerprint="coverage:${rel_path}"

            local description="Test coverage below threshold detected for **${rel_path}**.

**Coverage:** ${pct_int}%
**Threshold:** ${threshold}%

## Context
Low test coverage increases the risk of bugs going undetected. This file needs more comprehensive tests.

## Remediation
1. Run \`pytest --cov=${rel_path} --cov-report=term-missing\` to see uncovered lines
2. Write tests for untested functions
3. Focus on business logic and error handling"

            local issue
            issue=$(jq -n \
                --arg category "coverage" \
                --arg severity "medium" \
                --arg title "$title" \
                --arg description "$description" \
                --arg fingerprint "$fingerprint" \
                --arg labels "testing,coverage,python" \
                '{
                    category: $category,
                    severity: $severity,
                    title: $title,
                    description: $description,
                    fingerprint: $fingerprint,
                    labels: $labels
                }')

            issues=$(echo "$issues" "$issue" | jq -s 'add' 2>/dev/null || echo "$issues")
        fi

        ((idx++))
    done

    echo "$issues"
}

# Main test coverage detector
# Usage: _pulse_detector_coverage <workspace> <agent>
# Returns: JSON array of coverage issue objects
_pulse_detector_coverage() {
    local workspace="$1"
    local agent="$2"

    # Check if coverage detector is enabled
    local coverage_enabled
    coverage_enabled=$(get_config "strands.pulse.detectors.coverage" "false")

    if [[ "$coverage_enabled" != "true" ]]; then
        echo "[]"
        return 0
    fi

    _needle_diag_strand "pulse" "Running coverage gap detector" \
        "workspace=$workspace" \
        "agent=$agent"

    _needle_telemetry_emit "pulse.detector_started" "info" \
        "detector=coverage" \
        "workspace=$workspace"

    local all_issues="[]"

    # Run npm/jest coverage for Node.js projects
    local npm_issues
    npm_issues=$(_pulse_npm_coverage "$workspace")
    if [[ -n "$npm_issues" ]] && [[ "$npm_issues" != "[]" ]]; then
        all_issues=$(echo "$all_issues" "$npm_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
    fi

    # Run pytest coverage for Python projects
    local pytest_issues
    pytest_issues=$(_pulse_pytest_coverage "$workspace")
    if [[ -n "$pytest_issues" ]] && [[ "$pytest_issues" != "[]" ]]; then
        all_issues=$(echo "$all_issues" "$pytest_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
    fi

    local issue_count
    issue_count=$(echo "$all_issues" | jq 'length' 2>/dev/null || echo 0)

    _needle_telemetry_emit "pulse.detector_completed" "info" \
        "detector=coverage" \
        "workspace=$workspace" \
        "issues_found=$issue_count"

    _needle_diag_strand "pulse" "Coverage gap detector completed" \
        "workspace=$workspace" \
        "issues_found=$issue_count"

    echo "$all_issues"
}

# ============================================================================
# Stale TODO Detector (nd-gn2)
# ============================================================================

# Get the age of a line in days using git blame
# Usage: _pulse_get_line_age <file> <line_number>
# Returns: Age in days, or -1 if unavailable
_pulse_get_line_age() {
    local file="$1"
    local line_num="$2"

    # Check if file is in a git repo
    if ! git -C "$(dirname "$file")" rev-parse --git-dir &>/dev/null; then
        echo -1
        return 0
    fi

    # Get the timestamp of the last change to this line
    local timestamp
    timestamp=$(git -C "$(dirname "$file")" blame -L "$line_num,$line_num" --format="%(committerdate:unix)" "$file" 2>/dev/null | head -1)

    if [[ -z "$timestamp" ]] || ! [[ "$timestamp" =~ ^[0-9]+$ ]]; then
        echo -1
        return 0
    fi

    local now
    now=$(date +%s)
    local age_seconds=$((now - timestamp))
    local age_days=$((age_seconds / 86400))

    echo "$age_days"
}

# Scan files for stale TODO/FIXME comments
# Usage: _pulse_scan_todos <workspace>
# Returns: JSON array of TODO issue objects
_pulse_scan_todos() {
    local workspace="$1"
    local issues="[]"

    # Get TODO age threshold from config
    local age_threshold
    age_threshold=$(get_config "strands.pulse.todo_age_days" "180")

    # Find files with TODO/FIXME comments
    local todo_files=()
    while IFS= read -r -d '' file; do
        todo_files+=("$file")
    done < <(find "$workspace" -type f \( \
        -name "*.sh" -o -name "*.py" -o -name "*.js" -o -name "*.ts" -o \
        -name "*.go" -o -name "*.rs" -o -name "*.java" -o -name "*.rb" -o \
        -name "*.php" -o -name "*.c" -o -name "*.cpp" -o -name "*.h" \
        \) -not -path "*/node_modules/*" -not -path "*/.git/*" -not -path "*/venv/*" \
        -not -path "*/dist/*" -not -path "*/build/*" -print0 2>/dev/null | head -100)

    if [[ ${#todo_files[@]} -eq 0 ]]; then
        echo "[]"
        return 0
    fi

    local max_todos
    max_todos=$(get_config "strands.pulse.max_todos_per_run" "10")
    local todos_found=0

    for file in "${todo_files[@]}"; do
        # Stop if we've hit the limit
        if ((todos_found >= max_todos)); then
            break
        fi

        local rel_path="${file#$workspace/}"

        # Skip test files for TODO detection (they often have intentional TODOs)
        if [[ "$rel_path" == *"test"* ]] || [[ "$rel_path" == *"spec"* ]]; then
            continue
        fi

        # Find TODO/FIXME lines
        local line_num=0
        while IFS= read -r line; do
            ((line_num++))

            # Check for TODO or FIXME pattern
            if [[ "$line" =~ (TODO|FIXME|XXX|HACK)[[:space:]]*(:|=|\[) ]]; then
                local age_days
                age_days=$(_pulse_get_line_age "$file" "$line_num")

                # Skip if age couldn't be determined or is below threshold
                if [[ "$age_days" -lt 0 ]] || ((age_days < age_threshold)); then
                    continue
                fi

                # Extract TODO content (remove leading whitespace and comment markers)
                local todo_content="$line"
                todo_content="${todo_content#*TODO}"
                todo_content="${todo_content#*FIXME}"
                todo_content="${todo_content#*XXX}"
                todo_content="${todo_content#*HACK}"
                todo_content="${todo_content#[:=[]}"
                todo_content="${todo_content#"${todo_content%%[![:space:]]*}"}"  # Trim leading space
                todo_content="${todo_content:0:80}"  # Truncate to 80 chars

                local title="Stale TODO (${age_days}d old): $rel_path:$line_num"
                local fingerprint="stale-todo:${rel_path}:${line_num}"

                local description="Stale TODO/FIXME comment detected in **${rel_path}**.

**Age:** ${age_days} days (threshold: ${age_threshold} days)
**Line:** ${line_num}
**Content:** \`${todo_content}\`

## Context
This TODO comment has been in the codebase for over ${age_threshold} days without being addressed. Stale TODOs accumulate and become outdated, making them unreliable indicators of actual work needed.

## Remediation
1. Review if the TODO is still relevant
2. If relevant, create a proper issue/task to track it
3. If no longer needed, remove the comment
4. If blocked, document why and what's needed to unblock"

                local severity="low"
                if ((age_days >= 365)); then
                    severity="medium"
                fi

                local issue
                issue=$(jq -n \
                    --arg category "todos" \
                    --arg severity "$severity" \
                    --arg title "$title" \
                    --arg description "$description" \
                    --arg fingerprint "$fingerprint" \
                    --arg labels "todo,tech-debt,stale" \
                    '{
                        category: $category,
                        severity: $severity,
                        title: $title,
                        description: $description,
                        fingerprint: $fingerprint,
                        labels: $labels
                    }')

                issues=$(echo "$issues" "$issue" | jq -s 'add' 2>/dev/null || echo "$issues")
                ((todos_found++))
            fi
        done < "$file"
    done

    echo "$issues"
}

# Main stale TODO detector
# Usage: _pulse_detector_todos <workspace> <agent>
# Returns: JSON array of TODO issue objects
_pulse_detector_todos() {
    local workspace="$1"
    local agent="$2"

    # Check if TODO detector is enabled
    local todos_enabled
    todos_enabled=$(get_config "strands.pulse.detectors.todos" "true")

    if [[ "$todos_enabled" != "true" ]]; then
        echo "[]"
        return 0
    fi

    _needle_diag_strand "pulse" "Running stale TODO detector" \
        "workspace=$workspace" \
        "agent=$agent"

    _needle_telemetry_emit "pulse.detector_started" "info" \
        "detector=todos" \
        "workspace=$workspace"

    local issues
    issues=$(_pulse_scan_todos "$workspace")

    local issue_count
    issue_count=$(echo "$issues" | jq 'length' 2>/dev/null || echo 0)

    _needle_telemetry_emit "pulse.detector_completed" "info" \
        "detector=todos" \
        "workspace=$workspace" \
        "issues_found=$issue_count"

    _needle_diag_strand "pulse" "Stale TODO detector completed" \
        "workspace=$workspace" \
        "issues_found=$issue_count"

    echo "$issues"
}

# ============================================================================
# Linter Issues Detector (nd-1oaq)
# ============================================================================

# Run eslint and collect errors
# Usage: _pulse_run_eslint <workspace>
# Returns: JSON array of lint issue objects
_pulse_run_eslint() {
    local workspace="$1"
    local issues="[]"

    # Check if eslint is available
    if ! command -v npx &>/dev/null; then
        echo "[]"
        return 0
    fi

    # Check for package.json and eslint config
    if [[ ! -f "$workspace/package.json" ]]; then
        echo "[]"
        return 0
    fi

    # Check for eslint config files
    local has_eslint_config=false
    for config in .eslintrc .eslintrc.js .eslintrc.json .eslintrc.yml .eslintrc.yaml eslint.config.js; do
        if [[ -f "$workspace/$config" ]]; then
            has_eslint_config=true
            break
        fi
    done

    if [[ "$has_eslint_config" != "true" ]]; then
        echo "[]"
        return 0
    fi

    # Run eslint in JSON format, only errors
    local eslint_output
    eslint_output=$(cd "$workspace" && npx eslint --format json --quiet . 2>/dev/null) || true

    if [[ -z "$eslint_output" ]] || [[ "$eslint_output" == "[]" ]]; then
        echo "[]"
        return 0
    fi

    # Parse eslint output
    local max_issues=5
    local count=0

    while IFS= read -r file_result && ((count < max_issues)); do
        [[ -z "$file_result" ]] && continue

        local file_path
        file_path=$(echo "$file_result" | jq -r '.filePath // empty' 2>/dev/null)
        [[ -z "$file_path" ]] && continue

        local rel_path="${file_path#$workspace/}"

        # Get error messages
        while IFS= read -r msg && ((count < max_issues)); do
            [[ -z "$msg" ]] || [[ "$msg" == "null" ]] && continue

            local line rule message
            line=$(echo "$msg" | jq -r '.line // 0' 2>/dev/null)
            rule=$(echo "$msg" | jq -r '.ruleId // "unknown"' 2>/dev/null)
            message=$(echo "$msg" | jq -r '.message // "Unknown error"' 2>/dev/null)

            local title="ESLint error: $rel_path:$line"
            local fingerprint="eslint:${rel_path}:${line}:${rule}"
            local description="ESLint error in **${rel_path}**

**Rule:** ${rule}
**Line:** ${line}
**Message:** ${message}

## Context
This is a static analysis error detected by ESLint. Errors indicate potential bugs or problematic patterns that should be fixed.

## Remediation
1. Open the file and review the error
2. Fix the issue according to the rule documentation
3. Run \`npx eslint ${rel_path}\` to verify the fix"

            local issue
            issue=$(jq -n \
                --arg category "linter" \
                --arg severity "high" \
                --arg title "$title" \
                --arg description "$description" \
                --arg fingerprint "$fingerprint" \
                --arg labels "eslint,static-analysis" \
                '{
                    category: $category,
                    severity: $severity,
                    title: $title,
                    description: $description,
                    fingerprint: $fingerprint,
                    labels: $labels
                }')

            issues=$(echo "$issues" "$issue" | jq -s 'add' 2>/dev/null || echo "$issues")
            ((count++))
        done < <(echo "$file_result" | jq -c '.messages[] | select(.severity == 2)' 2>/dev/null)
    done < <(echo "$eslint_output" | jq -c '.[]' 2>/dev/null)

    echo "$issues"
}

# Run shellcheck and collect errors
# Usage: _pulse_run_shellcheck <workspace>
# Returns: JSON array of lint issue objects
_pulse_run_shellcheck() {
    local workspace="$1"
    local issues="[]"

    # Check if shellcheck is available
    if ! command -v shellcheck &>/dev/null; then
        echo "[]"
        return 0
    fi

    # Find shell scripts
    local shell_scripts=()
    while IFS= read -r -d '' file; do
        shell_scripts+=("$file")
    done < <(find "$workspace" -type f \( -name "*.sh" -o -name "*.bash" \) \
        -not -path "*/node_modules/*" -not -path "*/.git/*" -not -path "*/venv/*" \
        -print0 2>/dev/null | head -50)

    if [[ ${#shell_scripts[@]} -eq 0 ]]; then
        echo "[]"
        return 0
    fi

    local max_issues=5
    local count=0

    for script in "${shell_scripts[@]}"; do
        if ((count >= max_issues)); then
            break
        fi

        local rel_path="${script#$workspace/}"

        # Run shellcheck with JSON format
        local shellcheck_output
        shellcheck_output=$(shellcheck -f json "$script" 2>/dev/null) || true

        if [[ -z "$shellcheck_output" ]] || [[ "$shellcheck_output" == "[]" ]]; then
            continue
        fi

        # Parse errors only (level: error)
        while IFS= read -r result && ((count < max_issues)); do
            [[ -z "$result" ]] || [[ "$result" == "null" ]] && continue

            local level line code message
            level=$(echo "$result" | jq -r '.level // "info"' 2>/dev/null)
            line=$(echo "$result" | jq -r '.line // 0' 2>/dev/null)
            code=$(echo "$result" | jq -r '.code // "unknown"' 2>/dev/null)
            message=$(echo "$result" | jq -r '.message // "Unknown error"' 2>/dev/null)

            # Only process errors
            if [[ "$level" != "error" ]]; then
                continue
            fi

            local title="ShellCheck error: $rel_path:$line"
            local fingerprint="shellcheck:${rel_path}:${line}:${code}"
            local description="ShellCheck error in **${rel_path}**

**Code:** SC${code}
**Line:** ${line}
**Message:** ${message}

## Context
This is a static analysis error detected by ShellCheck. Errors indicate potential bugs or unsafe patterns in shell scripts.

## Remediation
1. Open the file and review the error
2. Fix the issue according to ShellCheck recommendations
3. Run \`shellcheck ${rel_path}\` to verify the fix"

            local severity="high"

            local issue
            issue=$(jq -n \
                --arg category "linter" \
                --arg severity "$severity" \
                --arg title "$title" \
                --arg description "$description" \
                --arg fingerprint "$fingerprint" \
                --arg labels "shellcheck,static-analysis,shell" \
                '{
                    category: $category,
                    severity: $severity,
                    title: $title,
                    description: $description,
                    fingerprint: $fingerprint,
                    labels: $labels
                }')

            issues=$(echo "$issues" "$issue" | jq -s 'add' 2>/dev/null || echo "$issues")
            ((count++))
        done < <(echo "$shellcheck_output" | jq -c '.[]' 2>/dev/null)
    done

    echo "$issues"
}

# Run ruff/pyflakes for Python linting
# Usage: _pulse_run_python_lint <workspace>
# Returns: JSON array of lint issue objects
_pulse_run_python_lint() {
    local workspace="$1"
    local issues="[]"

    # Check for Python files
    local python_files=()
    while IFS= read -r -d '' file; do
        python_files+=("$file")
    done < <(find "$workspace" -type f -name "*.py" \
        -not -path "*/node_modules/*" -not -path "*/.git/*" -not -path "*/venv/*" \
        -not -path "*/__pycache__/*" -not -path "*/.venv/*" \
        -print0 2>/dev/null | head -100)

    if [[ ${#python_files[@]} -eq 0 ]]; then
        echo "[]"
        return 0
    fi

    local max_issues=5
    local count=0

    # Try ruff first (faster)
    if command -v ruff &>/dev/null; then
        local ruff_output
        ruff_output=$(cd "$workspace" && ruff check --output-format json . 2>/dev/null) || true

        if [[ -n "$ruff_output" ]] && [[ "$ruff_output" != "[]" ]]; then
            while IFS= read -r result && ((count < max_issues)); do
                [[ -z "$result" ]] || [[ "$result" == "null" ]] && continue

                local filename location code message
                filename=$(echo "$result" | jq -r '.filename // empty' 2>/dev/null)
                location=$(echo "$result" | jq -r '.location // empty' 2>/dev/null)
                code=$(echo "$result" | jq -r '.code // "unknown"' 2>/dev/null)
                message=$(echo "$result" | jq -r '.message // "Unknown error"' 2>/dev/null)

                [[ -z "$filename" ]] && continue

                local rel_path="${filename#$workspace/}"
                local line="${location%%:*}"

                local title="Ruff error: $rel_path:$line"
                local fingerprint="ruff:${rel_path}:${location}:${code}"

                local description="Ruff lint error in **${rel_path}**

**Code:** ${code}
**Location:** ${location}
**Message:** ${message}

## Context
This is a static analysis error detected by Ruff. It indicates a potential issue in Python code.

## Remediation
1. Open the file and review the error
2. Fix the issue according to the rule documentation
3. Run \`ruff check ${rel_path}\` to verify the fix"

                local issue
                issue=$(jq -n \
                    --arg category "linter" \
                    --arg severity "high" \
                    --arg title "$title" \
                    --arg description "$description" \
                    --arg fingerprint "$fingerprint" \
                    --arg labels "ruff,static-analysis,python" \
                    '{
                        category: $category,
                        severity: $severity,
                        title: $title,
                        description: $description,
                        fingerprint: $fingerprint,
                        labels: $labels
                    }')

                issues=$(echo "$issues" "$issue" | jq -s 'add' 2>/dev/null || echo "$issues")
                ((count++))
            done < <(echo "$ruff_output" | jq -c '.[]' 2>/dev/null)
        fi
    fi

    echo "$issues"
}

# Main linter issues detector
# Usage: _pulse_detector_linter <workspace> <agent>
# Returns: JSON array of linter issue objects
_pulse_detector_linter() {
    local workspace="$1"
    local agent="$2"

    # Check if linter detector is enabled
    local linter_enabled
    linter_enabled=$(get_config "strands.pulse.detectors.linter_issues" "true")

    if [[ "$linter_enabled" != "true" ]]; then
        echo "[]"
        return 0
    fi

    # Get severity filter (only create beads for specified severity and above)
    local min_severity
    min_severity=$(get_config "strands.pulse.detectors.linter_severity" "error")

    _needle_diag_strand "pulse" "Running linter issues detector" \
        "workspace=$workspace" \
        "agent=$agent" \
        "min_severity=$min_severity"

    _needle_telemetry_emit "pulse.detector_started" "info" \
        "detector=linter" \
        "workspace=$workspace"

    local all_issues="[]"

    # Run ESLint for JS/TS projects
    local eslint_issues
    eslint_issues=$(_pulse_run_eslint "$workspace")
    if [[ -n "$eslint_issues" ]] && [[ "$eslint_issues" != "[]" ]]; then
        all_issues=$(echo "$all_issues" "$eslint_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
    fi

    # Run ShellCheck for shell scripts
    local shellcheck_issues
    shellcheck_issues=$(_pulse_run_shellcheck "$workspace")
    if [[ -n "$shellcheck_issues" ]] && [[ "$shellcheck_issues" != "[]" ]]; then
        all_issues=$(echo "$all_issues" "$shellcheck_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
    fi

    # Run Python linter
    local python_issues
    python_issues=$(_pulse_run_python_lint "$workspace")
    if [[ -n "$python_issues" ]] && [[ "$python_issues" != "[]" ]]; then
        all_issues=$(echo "$all_issues" "$python_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
    fi

    local issue_count
    issue_count=$(echo "$all_issues" | jq 'length' 2>/dev/null || echo 0)

    _needle_telemetry_emit "pulse.detector_completed" "info" \
        "detector=linter" \
        "workspace=$workspace" \
        "issues_found=$issue_count"

    _needle_diag_strand "pulse" "Linter issues detector completed" \
        "workspace=$workspace" \
        "issues_found=$issue_count"

    echo "$all_issues"
}

# ============================================================================
# Dead Code Detector (nd-1oaq)
# ============================================================================

# Check for unused exports in TypeScript/JavaScript using ts-prune or similar
# Usage: _pulse_scan_dead_code_ts <workspace>
# Returns: JSON array of dead code issue objects
_pulse_scan_dead_code_ts() {
    local workspace="$1"
    local issues="[]"

    # Check for ts-prune availability
    if ! command -v npx &>/dev/null; then
        echo "[]"
        return 0
    fi

    # Check for TypeScript project
    if [[ ! -f "$workspace/tsconfig.json" ]] && [[ ! -f "$workspace/package.json" ]]; then
        echo "[]"
        return 0
    fi

    # ts-prune requires installation, skip if not available
    if ! npx ts-prune --help &>/dev/null; then
        echo "[]"
        return 0
    fi

    # Run ts-prune to find unused exports
    local tsprune_output
    tsprune_output=$(cd "$workspace" && npx ts-prune 2>/dev/null) || true

    if [[ -z "$tsprune_output" ]]; then
        echo "[]"
        return 0
    fi

    local max_issues=5
    local count=0

    # Parse ts-prune output (format: "path:line - symbolName")
    while IFS= read -r line && ((count < max_issues)); do
        [[ -z "$line" ]] && continue

        # Match lines like "src/utils.ts:10 - unusedFunction"
        if [[ "$line" =~ ^(.+):([0-9]+)\ -\ (.+)$ ]]; then
            local file_path="${BASH_REMATCH[1]}"
            local line_num="${BASH_REMATCH[2]}"
            local symbol="${BASH_REMATCH[3]}"

            local rel_path="${file_path#$workspace/}"

            local title="Unused export: ${symbol} in ${rel_path}"
            local fingerprint="dead-code:${rel_path}:${symbol}"
            local description="Potentially unused export in **${rel_path}**

**Symbol:** ${symbol}
**Line:** ${line_num}

## Context
This export was detected as potentially unused by ts-prune. Unused exports may indicate:
- Dead code that can be removed
- Code that should be used but isn't (potential bug)
- Exports intended for external use (false positive)

## Remediation
1. Verify if the export is truly unused (check for dynamic imports, external packages)
2. If unused, remove the export and related dead code
3. If used externally, document the public API
4. Run \`npx ts-prune\` to verify"

            local issue
            issue=$(jq -n \
                --arg category "dead-code" \
                --arg severity "low" \
                --arg title "$title" \
                --arg description "$description" \
                --arg fingerprint "$fingerprint" \
                --arg labels "dead-code,unused,cleanup" \
                '{
                    category: $category,
                    severity: $severity,
                    title: $title,
                    description: $description,
                    fingerprint: $fingerprint,
                    labels: $labels
                }')

            issues=$(echo "$issues" "$issue" | jq -s 'add' 2>/dev/null || echo "$issues")
            ((count++))
        fi
    done <<< "$tsprune_output"

    echo "$issues"
}

# Check for unused dependencies using depcheck
# Usage: _pulse_scan_unused_deps <workspace>
# Returns: JSON array of unused dependency issue objects
_pulse_scan_unused_deps() {
    local workspace="$1"
    local issues="[]"

    # Check for package.json
    if [[ ! -f "$workspace/package.json" ]]; then
        echo "[]"
        return 0
    fi

    # Check if depcheck is available
    if ! command -v npx &>/dev/null; then
        echo "[]"
        return 0
    fi

    # Run depcheck with JSON output
    local depcheck_output
    depcheck_output=$(cd "$workspace" && npx depcheck --json 2>/dev/null) || true

    if [[ -z "$depcheck_output" ]] || [[ "$depcheck_output" == "{}" ]]; then
        echo "[]"
        return 0
    fi

    local max_issues=5
    local count=0

    # Parse unused dependencies
    while IFS= read -r dep && ((count < max_issues)); do
        [[ -z "$dep" ]] || [[ "$dep" == "null" ]] && continue

        local title="Unused dependency: $dep"
        local fingerprint="unused-dep:${dep}"
        local description="Unused npm dependency detected: **${dep}**

## Context
This dependency is declared in package.json but does not appear to be used in the codebase. Unused dependencies:
- Increase bundle size
- Slow down npm install
- Add unnecessary security surface area
- Can cause confusion about project requirements

## Remediation
1. Verify the dependency is truly unused (check for dynamic imports, build tools)
2. If unused, remove from package.json: \`npm uninstall ${dep}\`
3. Run \`npm install\` to update lock file
4. Run \`npx depcheck\` to verify"

        local issue
        issue=$(jq -n \
            --arg category "dead-code" \
            --arg severity "medium" \
            --arg title "$title" \
            --arg description "$description" \
            --arg fingerprint "$fingerprint" \
            --arg labels "dead-code,dependencies,unused" \
            '{
                category: $category,
                severity: $severity,
                title: $title,
                description: $description,
                fingerprint: $fingerprint,
                labels: $labels
            }')

        issues=$(echo "$issues" "$issue" | jq -s 'add' 2>/dev/null || echo "$issues")
        ((count++))
    done < <(echo "$depcheck_output" | jq -r '.dependencies[]? // empty' 2>/dev/null)

    echo "$issues"
}

# Main dead code detector
# Usage: _pulse_detector_dead_code <workspace> <agent>
# Returns: JSON array of dead code issue objects
_pulse_detector_dead_code() {
    local workspace="$1"
    local agent="$2"

    # Check if dead code detector is enabled (disabled by default - requires tooling)
    local dead_code_enabled
    dead_code_enabled=$(get_config "strands.pulse.detectors.dead_code" "false")

    if [[ "$dead_code_enabled" != "true" ]]; then
        echo "[]"
        return 0
    fi

    _needle_diag_strand "pulse" "Running dead code detector" \
        "workspace=$workspace" \
        "agent=$agent"

    _needle_telemetry_emit "pulse.detector_started" "info" \
        "detector=dead_code" \
        "workspace=$workspace"

    local all_issues="[]"

    # Scan for unused exports
    local ts_issues
    ts_issues=$(_pulse_scan_dead_code_ts "$workspace")
    if [[ -n "$ts_issues" ]] && [[ "$ts_issues" != "[]" ]]; then
        all_issues=$(echo "$all_issues" "$ts_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
    fi

    # Scan for unused dependencies
    local dep_issues
    dep_issues=$(_pulse_scan_unused_deps "$workspace")
    if [[ -n "$dep_issues" ]] && [[ "$dep_issues" != "[]" ]]; then
        all_issues=$(echo "$all_issues" "$dep_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
    fi

    local issue_count
    issue_count=$(echo "$all_issues" | jq 'length' 2>/dev/null || echo 0)

    _needle_telemetry_emit "pulse.detector_completed" "info" \
        "detector=dead_code" \
        "workspace=$workspace" \
        "issues_found=$issue_count"

    _needle_diag_strand "pulse" "Dead code detector completed" \
        "workspace=$workspace" \
        "issues_found=$issue_count"

    echo "$all_issues"
}

# ============================================================================
# Issue Collection and Processing
# ============================================================================

# Collect issues from all detectors
# Returns: JSON array of issues sorted by severity
#
# Usage: _pulse_collect_issues <workspace> <agent>
# Returns: JSON array of issue objects
_pulse_collect_issues() {
    local workspace="$1"
    local agent="$2"

    local all_issues="[]"

    # Run each detector and collect issues
    # Detectors are implemented in separate files (nd-qpj-2, nd-qpj-3, nd-qpj-4)

    # Security scan detector (placeholder - implemented in nd-qpj-2)
    if declare -f _pulse_detector_security &>/dev/null; then
        local security_issues
        security_issues=$(_pulse_detector_security "$workspace" "$agent")
        if [[ -n "$security_issues" ]] && [[ "$security_issues" != "[]" ]]; then
            all_issues=$(echo "$all_issues" "$security_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        fi
    fi

    # Dependency freshness detector (implemented in nd-1fr)
    if declare -f _pulse_detector_dependencies &>/dev/null; then
        local dep_issues
        dep_issues=$(_pulse_detector_dependencies "$workspace" "$agent")
        if [[ -n "$dep_issues" ]] && [[ "$dep_issues" != "[]" ]]; then
            all_issues=$(echo "$all_issues" "$dep_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        fi
    fi

    # Documentation drift detector (placeholder - implemented in nd-qpj-4)
    if declare -f _pulse_detector_docs &>/dev/null; then
        local doc_issues
        doc_issues=$(_pulse_detector_docs "$workspace" "$agent")
        if [[ -n "$doc_issues" ]] && [[ "$doc_issues" != "[]" ]]; then
            all_issues=$(echo "$all_issues" "$doc_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        fi
    fi

    # Test coverage detector (implemented in nd-gn2)
    if declare -f _pulse_detector_coverage &>/dev/null; then
        local coverage_issues
        coverage_issues=$(_pulse_detector_coverage "$workspace" "$agent")
        if [[ -n "$coverage_issues" ]] && [[ "$coverage_issues" != "[]" ]]; then
            all_issues=$(echo "$all_issues" "$coverage_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        fi
    fi

    # Stale TODO detector (implemented in nd-gn2)
    if declare -f _pulse_detector_todos &>/dev/null; then
        local todo_issues
        todo_issues=$(_pulse_detector_todos "$workspace" "$agent")
        if [[ -n "$todo_issues" ]] && [[ "$todo_issues" != "[]" ]]; then
            all_issues=$(echo "$all_issues" "$todo_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        fi
    fi

    # Linter issues detector (implemented in nd-1oaq)
    if declare -f _pulse_detector_linter &>/dev/null; then
        local linter_issues
        linter_issues=$(_pulse_detector_linter "$workspace" "$agent")
        if [[ -n "$linter_issues" ]] && [[ "$linter_issues" != "[]" ]]; then
            all_issues=$(echo "$all_issues" "$linter_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        fi
    fi

    # Dead code detector (implemented in nd-1oaq)
    if declare -f _pulse_detector_dead_code &>/dev/null; then
        local dead_code_issues
        dead_code_issues=$(_pulse_detector_dead_code "$workspace" "$agent")
        if [[ -n "$dead_code_issues" ]] && [[ "$dead_code_issues" != "[]" ]]; then
            all_issues=$(echo "$all_issues" "$dead_code_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        fi
    fi

    # Sort issues by severity (critical=0, high=1, medium=2, low=3)
    all_issues=$(echo "$all_issues" | jq 'sort_by(.severity | {critical: 0, high: 1, medium: 2, low: 3}[.] // 2)' 2>/dev/null || echo "[]")

    echo "$all_issues"
}

# Process collected issues and create beads up to max limit
#
# Usage: _pulse_process_issues <workspace> <issues_json>
# Returns: Number of beads created
_pulse_process_issues() {
    local workspace="$1"
    local issues="$2"

    local max_beads
    max_beads=$(get_config "strands.pulse.max_beads_per_run" "5")

    local created=0
    local processed=0

    # Process each issue up to max_beads limit
    while IFS= read -r issue && ((created < max_beads)); do
        [[ -z "$issue" ]] && continue
        [[ "$issue" == "null" ]] && continue

        ((processed++))

        # Extract issue fields
        local category title description fingerprint severity extra_labels

        if _needle_command_exists jq; then
            category=$(echo "$issue" | jq -r '.category // "general"' 2>/dev/null)
            title=$(echo "$issue" | jq -r '.title // empty' 2>/dev/null)
            description=$(echo "$issue" | jq -r '.description // empty' 2>/dev/null)
            fingerprint=$(echo "$issue" | jq -r '.fingerprint // empty' 2>/dev/null)
            severity=$(echo "$issue" | jq -r '.severity // "medium"' 2>/dev/null)
            extra_labels=$(echo "$issue" | jq -r '.labels // empty' 2>/dev/null)
        else
            continue
        fi

        # Skip if no title
        if [[ -z "$title" ]]; then
            _needle_debug "pulse: skipping issue with no title"
            continue
        fi

        # Generate fingerprint if not provided
        if [[ -z "$fingerprint" ]]; then
            fingerprint="$category:$title"
        fi

        # Create the bead
        if _pulse_create_bead "$workspace" "$category" "$title" "$description" "$fingerprint" "$severity" "$extra_labels"; then
            ((created++))
        fi
    done < <(echo "$issues" | jq -c '.[]' 2>/dev/null)

    _needle_diag_strand "pulse" "Processed issues" \
        "workspace=$workspace" \
        "issues_processed=$processed" \
        "beads_created=$created" \
        "max_beads=$max_beads"

    echo "$created"
}

# ============================================================================
# Main Strand Entry Point
# ============================================================================

_needle_strand_pulse() {
    local workspace="$1"
    local agent="$2"

    _needle_diag_strand "pulse" "Pulse strand started" \
        "workspace=$workspace" \
        "agent=$agent" \
        "session=${NEEDLE_SESSION:-unknown}"

    _needle_debug "pulse strand: checking codebase health in $workspace"

    # Check if workspace exists
    if [[ ! -d "$workspace" ]]; then
        _needle_debug "pulse: workspace does not exist: $workspace"
        return 1
    fi

    # Check frequency limit (don't run every loop)
    if ! _pulse_should_run "$workspace"; then
        _needle_debug "pulse: frequency limit not reached, skipping"
        return 1
    fi

    # Clean old seen issues
    _pulse_clean_seen_issues "$workspace"

    # Collect issues from all detectors
    local issues
    issues=$(_pulse_collect_issues "$workspace" "$agent")

    # Count issues
    local issue_count
    issue_count=$(echo "$issues" | jq 'length' 2>/dev/null || echo 0)

    if [[ -z "$issues" ]] || [[ "$issues" == "[]" ]] || [[ "$issue_count" -eq 0 ]]; then
        _needle_debug "pulse: no issues detected"

        # Record scan even when no issues found
        _pulse_record_scan "$workspace"

        _needle_telemetry_emit "pulse.scan_completed" "info" \
            "workspace=$workspace" \
            "issues_found=0" \
            "beads_created=0"

        return 1
    fi

    _needle_verbose "pulse: found $issue_count issue(s)"

    # Process issues and create beads
    local created
    created=$(_pulse_process_issues "$workspace" "$issues")

    # Record scan completion
    _pulse_record_scan "$workspace"

    if [[ "$created" -gt 0 ]]; then
        _needle_success "pulse: created $created bead(s) from health scan"

        # Emit completion event
        _needle_telemetry_emit "pulse.scan_completed" "info" \
            "workspace=$workspace" \
            "issues_found=$issue_count" \
            "beads_created=$created"

        return 0
    fi

    _needle_debug "pulse: no beads created (all issues were duplicates or filtered)"
    return 1
}

# ============================================================================
# Utility Functions
# ============================================================================

# Get statistics about pulse strand activity
# Usage: _pulse_stats
# Returns: JSON object with stats
_pulse_stats() {
    local state_dir
    state_dir=$(_pulse_state_dir)

    local scan_count=0
    local seen_count=0
    local last_scan="never"

    if [[ -d "$state_dir" ]]; then
        # Count scan tracking files
        scan_count=$(find "$state_dir" -name "last_scan_*.json" -type f 2>/dev/null | wc -l)

        # Count seen issues
        seen_count=$(find "$state_dir" -name "seen_issues_*.jsonl" -type f -exec cat {} \; 2>/dev/null | wc -l)

        # Get most recent scan time
        local newest_file
        newest_file=$(find "$state_dir" -name "last_scan_*.json" -type f -printf '%T@ %p\n' 2>/dev/null | sort -rn | head -1 | cut -d' ' -f2-)

        if [[ -n "$newest_file" ]] && [[ -f "$newest_file" ]]; then
            last_scan=$(jq -r '.last_scan_iso // "unknown"' "$newest_file" 2>/dev/null || echo "unknown")
        fi
    fi

    _needle_json_object \
        "workspace_tracking_files=$scan_count" \
        "total_seen_issues=$seen_count" \
        "last_scan=$last_scan"
}

# Clear pulse rate limit for a workspace (for testing/manual intervention)
# Usage: _pulse_clear_rate_limit <workspace>
_pulse_clear_rate_limit() {
    local workspace="$1"

    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir
    state_dir=$(_pulse_state_dir)
    local last_scan_file="$state_dir/last_scan_${workspace_hash}.json"

    if [[ -f "$last_scan_file" ]]; then
        rm -f "$last_scan_file"
        _needle_info "Cleared pulse rate limit for: $workspace"
    fi
}

# Manually trigger pulse scan for testing
# Usage: _pulse_run <workspace> [agent]
_pulse_run() {
    local workspace="$1"
    local agent="${2:-default}"

    # Clear rate limit to force run
    _pulse_clear_rate_limit "$workspace"

    # Run pulse
    _needle_strand_pulse "$workspace" "$agent"
}

# Reset pulse state for a workspace (clears all seen issues)
# Usage: _pulse_reset <workspace>
_pulse_reset() {
    local workspace="$1"

    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir
    state_dir=$(_pulse_state_dir)

    # Remove all state files for this workspace
    rm -f "$state_dir/last_scan_${workspace_hash}.json"
    rm -f "$state_dir/state_${workspace_hash}.json"
    rm -f "$state_dir/seen_issues_${workspace_hash}.jsonl"

    _needle_info "Reset pulse state for: $workspace"
}
