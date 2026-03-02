#!/usr/bin/env bash
# NEEDLE Strand: knot (Priority 7)
# Alert human when stuck
#
# Implementation: nd-d2a
#
# This strand is the last resort when all other strands find no work.
# It alerts a human that the system is stuck and needs intervention.
#
# Usage:
#   _needle_strand_knot <workspace> <agent>
#
# Return values:
#   0 - Alert was sent successfully
#   1 - Alert failed or rate-limited

# ============================================================================
# Main Strand Entry Point
# ============================================================================

_needle_strand_knot() {
    local workspace="$1"
    local agent="$2"

    _needle_debug "knot strand: checking if human alert is needed for $workspace"

    # Check rate limit - only alert once per hour per workspace
    if ! _needle_knot_check_rate_limit "$workspace"; then
        _needle_debug "knot strand: rate limited, skipping alert"
        return 1
    fi

    # Check if there's already an open needle-stuck alert for this workspace
    if _needle_knot_has_existing_alert "$workspace"; then
        _needle_debug "knot strand: existing alert found, skipping"
        return 1
    fi

    # Create the alert bead
    if _needle_knot_create_alert "$workspace" "$agent"; then
        _needle_debug "knot strand: alert created successfully"
        return 0
    fi

    _needle_debug "knot strand: failed to create alert"
    return 1
}

# ============================================================================
# Rate Limiting
# ============================================================================

# Check if we should rate-limit alerts for this workspace
# Returns: 0 if we can proceed (not rate limited), 1 if rate limited
_needle_knot_check_rate_limit() {
    local workspace="$1"

    # Get rate limit interval from config (default: 1 hour = 3600 seconds)
    local rate_limit_interval
    rate_limit_interval=$(get_config "knot.rate_limit_interval" "3600")

    # Create a unique identifier for this workspace
    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR"
    local last_alert_file="$state_dir/knot_alert_${workspace_hash}"

    # Ensure state directory exists
    mkdir -p "$state_dir"

    # Check if last alert file exists
    if [[ -f "$last_alert_file" ]]; then
        local last_ts
        last_ts=$(cat "$last_alert_file" 2>/dev/null)

        # Validate timestamp
        if [[ -n "$last_ts" ]] && [[ "$last_ts" =~ ^[0-9]+$ ]]; then
            local now
            now=$(date +%s)
            local elapsed=$((now - last_ts))

            if ((elapsed < rate_limit_interval)); then
                _needle_verbose "knot: rate limited (${elapsed}s since last alert, need ${rate_limit_interval}s)"
                return 1
            fi
        fi
    fi

    return 0
}

# Record that we sent an alert for this workspace
_needle_knot_record_alert() {
    local workspace="$1"

    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR"
    local last_alert_file="$state_dir/knot_alert_${workspace_hash}"

    # Record current timestamp
    date +%s > "$last_alert_file"
}

# ============================================================================
# Existing Alert Detection
# ============================================================================

# Check if there's already an open needle-stuck alert for this workspace
# Returns: 0 if existing alert found, 1 if no existing alert
_needle_knot_has_existing_alert() {
    local workspace="$1"

    # Look for open human beads with needle-stuck label in the workspace
    local existing
    existing=$(br list --workspace="$workspace" --status open --type human --json 2>/dev/null | \
               jq -r '.[] | select(.labels // [] | contains(["needle-stuck"])) | .id' 2>/dev/null | \
               head -1)

    if [[ -n "$existing" ]]; then
        _needle_debug "knot: found existing needle-stuck alert: $existing"
        return 0
    fi

    return 1
}

# ============================================================================
# Alert Creation
# ============================================================================

# Create a human alert bead for stuck state
# Returns: 0 on success, 1 on failure
_needle_knot_create_alert() {
    local workspace="$1"
    local agent="$2"

    _needle_info "knot: creating stuck alert for workspace: $workspace"

    # Collect diagnostic information
    local diag_info
    diag_info=$(_needle_knot_collect_diagnostics "$workspace" "$agent")

    # Build the alert title
    local title="NEEDLE stuck: no work found in $workspace"

    # Build the description with diagnostic context
    local description
    description=$(cat << EOF
## NEEDLE Stuck Alert

The NEEDLE worker has exhausted all 7 strands without finding work.
This indicates the system is in a stuck state requiring human attention.

### Context
- **Workspace:** $workspace
- **Agent:** $agent
- **Timestamp:** $(date -u +%Y-%m-%dT%H:%M:%SZ)

### Diagnostic Information

$diag_info

---

*This is an automated alert from NEEDLE Strand 7 (knot)*
EOF
)

    # Create the human alert bead
    local bead_id
    bead_id=$(br create \
        --title "$title" \
        --type human \
        --priority 0 \
        --labels "alert,needle-stuck" \
        --description "$description" \
        --silent 2>/dev/null)

    if [[ $? -eq 0 ]] && [[ -n "$bead_id" ]]; then
        _needle_success "knot: created alert bead: $bead_id"

        # Record the alert for rate limiting
        _needle_knot_record_alert "$workspace"

        # Emit telemetry event
        _needle_emit_event "strand.knot.alert_created" \
            "Created stuck alert bead" \
            "bead_id=$bead_id" \
            "workspace=$workspace" \
            "agent=$agent"

        return 0
    fi

    _needle_warn "knot: failed to create alert bead"
    return 1
}

# ============================================================================
# Diagnostics Collection
# ============================================================================

# Collect diagnostic information for the alert
# Returns: Multi-line diagnostic text (printed to stdout)
_needle_knot_collect_diagnostics() {
    local workspace="$1"
    local agent="$2"

    # 1. Recent events from log file
    echo "### Recent Events"
    echo ""
    local log_file="$NEEDLE_HOME/$NEEDLE_LOG_DIR/$(date +%Y-%m-%d).jsonl"
    if [[ -f "$log_file" ]]; then
        echo '```'
        tail -20 "$log_file" 2>/dev/null | while IFS= read -r line; do
            if _needle_command_exists jq; then
                local ts event msg
                ts=$(echo "$line" | jq -r '.ts // "unknown"' 2>/dev/null)
                event=$(echo "$line" | jq -r '.event // "unknown"' 2>/dev/null)
                msg=$(echo "$line" | jq -r '.message // ""' 2>/dev/null)
                echo "$ts [$event] $msg"
            else
                echo "$line"
            fi
        done
        echo '```'
    else
        echo "No log file found at $log_file"
    fi

    # 2. Workspace bead summary
    echo ""
    echo "### Workspace Bead Summary"
    echo ""
    local bead_summary
    bead_summary=$(br list --workspace="$workspace" --json 2>/dev/null)
    if [[ -n "$bead_summary" ]] && [[ "$bead_summary" != "[]" ]] && [[ "$bead_summary" != "null" ]]; then
        if _needle_command_exists jq; then
            echo '```'
            echo "$bead_summary" | jq -r 'group_by(.status) | .[] | "\(.[0].status): \(length) beads"' 2>/dev/null
            echo '```'
        else
            echo "Beads exist in workspace (summary requires jq)"
        fi
    else
        echo "No beads found or unable to retrieve bead summary"
    fi

    # 3. Worker status
    echo ""
    echo "### Active Workers"
    echo ""
    local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
    if [[ -d "$heartbeat_dir" ]]; then
        local heartbeat_count
        heartbeat_count=$(find "$heartbeat_dir" -name "*.json" -type f 2>/dev/null | wc -l)
        echo "- Active heartbeats: $heartbeat_count"

        # List active workers (up to 5)
        local workers
        workers=$(find "$heartbeat_dir" -name "*.json" -type f -exec basename {} .json \; 2>/dev/null | head -5)
        if [[ -n "$workers" ]]; then
            echo "- Recent workers: $(echo "$workers" | tr '\n' ', ' | sed 's/,$//')"
        fi
    else
        echo "- No heartbeat directory found"
    fi

    # 4. Strand status
    echo ""
    echo "### Strand Configuration"
    echo ""
    echo "| Strand | Enabled |"
    echo "|--------|--------|"
    for strand in pluck explore mend weave unravel pulse knot; do
        local enabled
        enabled=$(get_config "strands.$strand" "false")
        echo "| $strand | $enabled |"
    done

    # 5. Agent information
    echo ""
    echo "### Agent Information"
    echo ""
    echo "- **Session:** ${NEEDLE_SESSION:-unknown}"
    echo "- **Runner:** ${NEEDLE_RUNNER:-unknown}"
    echo "- **Provider:** ${NEEDLE_PROVIDER:-unknown}"
    echo "- **Model:** ${NEEDLE_MODEL:-unknown}"
    echo "- **Identifier:** ${NEEDLE_IDENTIFIER:-unknown}"
}

# ============================================================================
# Utility Functions
# ============================================================================

# Get statistics about knot strand activity
# Usage: _needle_knot_stats
# Returns: JSON object with stats
_needle_knot_stats() {
    local state_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR"

    local alert_count=0
    local last_alert="never"

    # Count alert tracking files
    if [[ -d "$state_dir" ]]; then
        alert_count=$(find "$state_dir" -name "knot_alert_*" -type f 2>/dev/null | wc -l)

        # Get most recent alert time
        local newest_file
        newest_file=$(find "$state_dir" -name "knot_alert_*" -type f -printf '%T@ %p\n' 2>/dev/null | sort -rn | head -1 | cut -d' ' -f2-)

        if [[ -n "$newest_file" ]] && [[ -f "$newest_file" ]]; then
            local ts
            ts=$(cat "$newest_file" 2>/dev/null)
            if [[ -n "$ts" ]] && [[ "$ts" =~ ^[0-9]+$ ]]; then
                last_alert=$(date -d "@$ts" -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo "$ts")
            fi
        fi
    fi

    _needle_json_object \
        "alert_tracking_files=$alert_count" \
        "last_alert=$last_alert"
}

# Clear rate limit for a workspace (for testing/manual intervention)
# Usage: _needle_knot_clear_rate_limit <workspace>
_needle_knot_clear_rate_limit() {
    local workspace="$1"

    local workspace_hash
    workspace_hash=$(echo "$workspace" | md5sum | cut -c1-8)

    local state_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR"
    local last_alert_file="$state_dir/knot_alert_${workspace_hash}"

    if [[ -f "$last_alert_file" ]]; then
        rm -f "$last_alert_file"
        _needle_info "Cleared knot rate limit for: $workspace"
    fi
}
