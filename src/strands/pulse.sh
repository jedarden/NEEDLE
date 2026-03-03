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

    # Create the bead
    local bead_id
    bead_id=$(br create \
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
        _needle_telemetry_emit "pulse.bead_created" \
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

    # Dependency freshness detector (placeholder - implemented in nd-qpj-3)
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

    # Test coverage detector (placeholder - implemented in nd-qpj-4)
    if declare -f _pulse_detector_coverage &>/dev/null; then
        local coverage_issues
        coverage_issues=$(_pulse_detector_coverage "$workspace" "$agent")
        if [[ -n "$coverage_issues" ]] && [[ "$coverage_issues" != "[]" ]]; then
            all_issues=$(echo "$all_issues" "$coverage_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
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

        _needle_telemetry_emit "pulse.scan_completed" \
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
        _needle_telemetry_emit "pulse.scan_completed" \
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
