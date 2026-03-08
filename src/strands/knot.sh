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

# Source bead claim module for _needle_create_bead
if [[ -z "${_NEEDLE_CLAIM_LOADED:-}" ]]; then
    NEEDLE_SRC="${NEEDLE_SRC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
    source "$NEEDLE_SRC/bead/claim.sh"
fi

# ============================================================================
# Main Strand Entry Point
# ============================================================================

_needle_strand_knot() {
    local workspace="$1"
    local agent="$2"

    _needle_debug "knot strand: checking if human alert is needed for $workspace"

    # PRE-FLIGHT VERIFICATION: Check if work is actually available
    # This prevents false positive starvation alerts when beads exist
    # but weren't found by earlier strands due to timing/race conditions
    if _needle_knot_verify_work_available "$workspace"; then
        _needle_debug "knot strand: pre-flight found available work - skipping alert (false positive prevented)"
        # Return 1 (no work) to maintain consistency - we didn't create an alert
        # But log that we found work for diagnostics
        _needle_emit_event "knot.false_positive_prevented" \
            "Pre-flight verification found available work, skipping starvation alert" \
            "workspace=$workspace"
        return 1
    fi

    # DB CORRUPTION CHECK (nd-1jv): Before creating starvation alert,
    # verify database integrity. WAL corruption (>10MB) causes queries to
    # return empty results, creating false "no work" starvation alerts.
    # Known false alarms: nd-6hc, nd-6qd, nd-2hf, nd-2jp, nd-ytw
    if ! _needle_knot_check_db_health "$workspace"; then
        # DB was corrupted and has been rebuilt - re-verify work availability
        _needle_debug "knot strand: DB corruption detected and repaired, re-checking work"
        if _needle_knot_verify_work_available "$workspace"; then
            _needle_debug "knot strand: work found after DB rebuild - false positive prevented"
            _needle_emit_event "knot.db_corruption_false_positive_prevented" \
                "DB corruption caused false starvation - rebuilt and found work" \
                "workspace=$workspace"
            return 1
        fi
        # DB was rebuilt but still no work - legitimate starvation, continue
        _needle_debug "knot strand: DB rebuilt but still no work - proceeding with alert"
    fi

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
# Pre-Flight Verification
# ============================================================================

# Pre-flight verification to prevent false positive starvation alerts
# Checks if work is actually available before creating an alert
# Returns: 0 if work IS available (should NOT alert), 1 if no work (should alert)
#
# Implementation note (nd-1ak): Uses multiple methods to detect claimable beads:
# 1. br ready --json (most accurate, accounts for dependencies)
# 2. needle-ready tool (fallback with dependency status checking)
# 3. Direct br list with comprehensive filtering (last resort)
#
# Implementation note (nd-1xl): Emits structured diagnostic event with verification
# results before creating HUMAN alert to prevent false positives like nd-3eh.
_needle_knot_verify_work_available() {
    local workspace="$1"

    _needle_debug "knot: running pre-flight verification for $workspace"

    # Initialize diagnostic counters (for nd-1xl verification event)
    local diag_br_ready_count=0
    local diag_needle_ready_count=0
    local diag_direct_count=0
    local diag_any_open=0
    local diag_claimed="?"
    local diag_assigned="?"
    local diag_blocked="?"
    local diag_deferred="?"
    local diag_human_type="?"
    local diag_has_deps="?"

    # Method 1: br ready --json (PRIMARY - most accurate)
    # This accounts for dependencies, claims, blocking, deferral, and human type
    # FIX (nd-20a): Also filter out beads with assignees - they can't be claimed by this worker
    # br ready may include assigned beads and human-type beads, so we need to exclude them
    # Using "not contains" pattern to avoid bash escaping issues with !=
    diag_br_ready_count=$(cd "$workspace" 2>/dev/null && br ready --json 2>/dev/null | \
        jq 'map(select(.assignee == null or .assignee == "") |
                   select(.issue_type == null or (.issue_type | contains("human") | not))) | length' 2>/dev/null || echo "0")

    if [[ "$diag_br_ready_count" -gt 0 ]]; then
        _needle_debug "knot: pre-flight found $diag_br_ready_count claimable beads via br ready (filtered for unassigned + non-human)"
        return 0  # Work available - DON'T create alert
    fi

    # Method 2: Check using needle-ready tool if available
    # needle-ready handles dependency status checking (nd-3jf fix)
    # FIX (nd-20a): Also filter for unassigned + non-human beads
    local needle_ready="$workspace/bin/needle-ready"
    if [[ -x "$needle_ready" ]]; then
        diag_needle_ready_count=$("$needle_ready" --json 2>/dev/null | \
            jq 'map(select(.assignee == null or .assignee == "") |
                       select(.issue_type == null or (.issue_type | contains("human") | not))) | length' 2>/dev/null || echo "0")

        if [[ "$diag_needle_ready_count" -gt 0 ]]; then
            _needle_debug "knot: pre-flight found $diag_needle_ready_count claimable beads via needle-ready (filtered for unassigned + non-human)"
            return 0  # Work available - DON'T create alert
        fi
    fi

    # Method 3: Direct br list check with comprehensive filtering
    # Filters: open status, unclaimed, not blocked, not deferred, not HUMAN type, not assigned
    # NOTE: Using length == 0 instead of == "" to avoid shell escaping issues with jq (nd-ane)
    diag_direct_count=$(cd "$workspace" 2>/dev/null && br list --status open --priority 0,1,2,3 --json 2>/dev/null | \
        jq 'map(select(.claimed_by | length == 0) |
                   select(.blocked_by | length == 0) |
                   select(.deferred_until | length == 0) |
                   select(.assignee | length == 0) |
                   select(.issue_type | length == 0 or . == "task")) | length' 2>/dev/null || echo "0")

    if [[ "$diag_direct_count" -gt 0 ]]; then
        _needle_debug "knot: pre-flight found $diag_direct_count claimable beads via direct query"
        return 0  # Work available - DON'T create alert
    fi

    # Method 4: Check for any open beads at all (diagnostic only)
    diag_any_open=$(cd "$workspace" 2>/dev/null && br list --status open --priority 0,1,2,3 --json 2>/dev/null | \
        jq 'length' 2>/dev/null || echo "0")

    # Also track assigned beads (different from claimed - persistent assignment)
    local diag_assigned=0

    if [[ "$diag_any_open" -gt 0 ]]; then
        # There are open beads but none are claimable
        # Log diagnostic info but don't alert (might be all claimed or blocked)
        _needle_debug "knot: found $diag_any_open open beads but none claimable - logging diagnostics"

        # Log why beads aren't claimable for debugging
        # NOTE: Using length > 0 instead of != "" to avoid shell escaping issues with jq (nd-ane)
        diag_claimed=$(cd "$workspace" && br list --status open --priority 0,1,2,3 --json 2>/dev/null | \
            jq 'map(select(.claimed_by | length > 0)) | length' 2>/dev/null || echo "?")
        diag_blocked=$(cd "$workspace" && br list --status open --priority 0,1,2,3 --json 2>/dev/null | \
            jq 'map(select(.blocked_by | length > 0)) | length' 2>/dev/null || echo "?")
        diag_deferred=$(cd "$workspace" && br list --status open --priority 0,1,2,3 --json 2>/dev/null | \
            jq 'map(select(.deferred_until | length > 0)) | length' 2>/dev/null || echo "?")
        diag_human_type=$(cd "$workspace" && br list --status open --priority 0,1,2,3 --json 2>/dev/null | \
            jq 'map(select(.issue_type == "human")) | length' 2>/dev/null || echo "?")
        diag_has_deps=$(cd "$workspace" && br list --status open --priority 0,1,2,3 --json 2>/dev/null | \
            jq 'map(select(.dependency_count > 0)) | length' 2>/dev/null || echo "?")

        # Check for assigned beads (persistent assignment vs temporary claim)
        # This detects when all work is assigned to specific workers - expected behavior
        diag_assigned=$(cd "$workspace" && br list --status open --priority 0,1,2,3 --json 2>/dev/null | \
            jq 'map(select(.assignee | length > 0)) | length' 2>/dev/null || echo "0")

        _needle_debug "knot: bead status - claimed: $diag_claimed, assigned: $diag_assigned, blocked: $diag_blocked, deferred: $diag_deferred, human: $diag_human_type, has_deps: $diag_has_deps"

        # FIX (nd-ane): If ALL beads are assigned to specific workers,
        # check if those workers are actually active (have fresh heartbeats).
        # If all assignees are active → skip alert (expected worker pool utilization).
        # If some assignees are stale/dead → proceed with alert (work may be stuck).
        if [[ "$diag_assigned" -gt 0 ]] && [[ "$diag_assigned" -ge "$diag_any_open" ]]; then
            # Collect unique assignees from the open beads
            local assignees
            assignees=$(cd "$workspace" && br list --status open --priority 0,1,2,3 --json 2>/dev/null | \
                jq -r '[.[].assignee | select(. != null and . != "")] | unique | .[]' 2>/dev/null)

            if [[ -n "$assignees" ]]; then
                local all_active=true
                local active_count=0
                local stale_count=0
                local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"

                while IFS= read -r assignee; do
                    [[ -z "$assignee" ]] && continue
                    if _needle_knot_is_worker_active "$assignee" "$heartbeat_dir"; then
                        ((active_count++))
                    else
                        ((stale_count++))
                        all_active=false
                    fi
                done <<< "$assignees"

                if [[ "$all_active" == "true" ]]; then
                    _needle_info "knot: all $diag_any_open open beads are assigned to $active_count active workers - expected behavior, skipping alert"
                    _needle_emit_event "knot.all_work_assigned" \
                        "All open beads are assigned to active workers - expected worker pool utilization" \
                        "workspace=$workspace" \
                        "assigned_count=$diag_assigned" \
                        "total_open=$diag_any_open" \
                        "active_workers=$active_count"
                    return 1  # No work available for THIS worker, but not an error
                else
                    _needle_warn "knot: all $diag_any_open beads assigned but $stale_count workers have stale/missing heartbeats - proceeding with alert"
                    _needle_emit_event "knot.assigned_workers_stale" \
                        "All beads assigned but some workers appear inactive" \
                        "workspace=$workspace" \
                        "assigned_count=$diag_assigned" \
                        "total_open=$diag_any_open" \
                        "active_workers=$active_count" \
                        "stale_workers=$stale_count"
                    # Fall through to alert creation
                fi
            else
                # Assigned but can't determine assignees - skip alert conservatively
                _needle_info "knot: all $diag_any_open open beads are assigned - skipping alert"
                _needle_emit_event "knot.all_work_assigned" \
                    "All open beads are assigned to specific workers - expected worker pool utilization" \
                    "workspace=$workspace" \
                    "assigned_count=$diag_assigned" \
                    "total_open=$diag_any_open"
                return 1
            fi
        fi

        # Still return 1 (no work available) since beads aren't claimable
        # But this is a legitimate reason, not a false positive
    fi

    # No work available - emit verification diagnostics before proceeding (nd-1xl)
    _needle_knot_emit_verification_diagnostics "$workspace" \
        "br_ready=$diag_br_ready_count" \
        "needle_ready=$diag_needle_ready_count" \
        "direct_query=$diag_direct_count" \
        "any_open=$diag_any_open" \
        "claimed=$diag_claimed" \
        "assigned=$diag_assigned" \
        "blocked=$diag_blocked" \
        "deferred=$diag_deferred" \
        "human_type=$diag_human_type" \
        "has_deps=$diag_has_deps"

    _needle_debug "knot: pre-flight confirmed no claimable work available"
    return 1
}

# ============================================================================
# Verification Diagnostics Emission (nd-1xl)
# ============================================================================

# Emit structured event with verification diagnostics before creating HUMAN alert
# This provides a clear audit trail of what was checked and found, preventing
# false positive alerts like nd-3eh where work was actually available.
#
# Usage: _needle_knot_emit_verification_diagnostics <workspace> [key=value ...]
_needle_knot_emit_verification_diagnostics() {
    local workspace="$1"
    shift

    # Emit the structured verification event
    _needle_emit_event "knot.verification_diagnostic" \
        "Starvation verification completed - diagnostic info before alert creation" \
        "workspace=$workspace" \
        "$@"
}

# ============================================================================
# Database Health Check (nd-1jv)
# ============================================================================

# Check database health before creating starvation alert.
# WAL corruption (>10MB) causes queries to return empty results, leading to
# false starvation alerts. This runs the maintenance health check script
# which auto-rebuilds the DB from JSONL if corruption is detected.
#
# Returns: 0 if DB is healthy, 1 if corruption was detected and fixed
_needle_knot_check_db_health() {
    local workspace="$1"

    _needle_debug "knot: running database health check for $workspace"

    local health_script="$workspace/.beads/maintenance/db-health-check.sh"

    if [[ ! -x "$health_script" ]]; then
        _needle_debug "knot: db-health-check.sh not found or not executable at $health_script"
        return 0  # No health check available, assume healthy
    fi

    local health_exit
    (cd "$workspace" && "$health_script") 2>&1 | while IFS= read -r line; do
        _needle_debug "knot: db-health: $line"
    done
    health_exit=${PIPESTATUS[0]}

    case $health_exit in
        0)
            _needle_debug "knot: database healthy"
            return 0
            ;;
        1)
            _needle_warn "knot: database corruption detected and auto-repaired"
            _needle_emit_event "knot.db_corruption_repaired" \
                "Database corruption detected during starvation check - auto-rebuilt from JSONL" \
                "workspace=$workspace"
            return 1
            ;;
        *)
            _needle_warn "knot: database health check failed with exit code $health_exit"
            _needle_emit_event "knot.db_health_check_error" \
                "Database health check returned error" \
                "workspace=$workspace" \
                "exit_code=$health_exit"
            return 0  # On error, proceed as if healthy (don't suppress legitimate alerts)
            ;;
    esac
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
    existing=$(br list --workspace="$workspace" --status open --priority 0,1,2,3 --type human --json 2>/dev/null | \
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

# Double-check if work is available before creating alert
# This is a "belt and suspenders" check to prevent stale alerts
# when work becomes available between pre-flight and alert creation.
#
# Returns: 0 if work IS available (should NOT alert), 1 if no work (can proceed)
#
# Implementation note (nd-kon): Uses br ready as the fastest, most reliable check.
# This is intentionally simple - we've already done comprehensive checks in pre-flight.
_needle_knot_double_check_work_available() {
    local workspace="$1"

    _needle_debug "knot: running double-check for $workspace"

    # Use br ready --json for a quick, accurate check
    # This accounts for dependencies, claims, blocking, deferral, and human type
    # FIX (nd-20a): Also filter for unassigned + non-human beads
    local count
    count=$(cd "$workspace" 2>/dev/null && br ready --json 2>/dev/null | \
        jq 'map(select(.assignee == null or .assignee == "") |
                   select(.issue_type == null or (.issue_type | contains("human") | not))) | length' 2>/dev/null || echo "0")

    if [[ "$count" -gt 0 ]]; then
        _needle_debug "knot: double-check found $count claimable beads"
        return 0  # Work available - DON'T create alert
    fi

    _needle_debug "knot: double-check confirmed no claimable work"
    return 1  # No work - can proceed with alert
}

# Create a human alert bead for stuck state
# Returns: 0 on success, 1 on failure
_needle_knot_create_alert() {
    local workspace="$1"
    local agent="$2"

    _needle_info "knot: creating stuck alert for workspace: $workspace"

    # BELT-AND-SUSPENDERS: Double-check that no work is available
    # This prevents stale alerts when the pre-flight check passed but
    # work became available between the check and alert creation.
    # (Implements nd-kon: Stale alert detection)
    if _needle_knot_double_check_work_available "$workspace"; then
        _needle_warn "knot: skipping alert - double-check found available work (stale alert prevented)"
        _needle_emit_event "knot.stale_alert_prevented" \
            "Double-check found available work, skipping alert creation" \
            "workspace=$workspace"
        return 1  # Don't create alert - work is available
    fi

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

    # Create the human alert bead using wrapper
    # Note: Human-type beads keep their assignment for visibility
    local bead_id
    bead_id=$(_needle_create_bead \
        --workspace "$workspace" \
        --title "$title" \
        --type human \
        --priority 0 \
        --label "alert" \
        --label "needle-stuck" \
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

# Check if a worker has an active (fresh) heartbeat
# A worker is considered active if it has a heartbeat file with a timestamp
# within the heartbeat timeout window (default: 120s, matching watchdog config).
#
# Usage: _needle_knot_is_worker_active <worker_name> <heartbeat_dir>
# Returns: 0 if active, 1 if stale/missing
#
# Implementation: nd-ane
_needle_knot_is_worker_active() {
    local worker_name="$1"
    local heartbeat_dir="$2"

    # Get heartbeat timeout from config (default: 120s, same as watchdog)
    local timeout
    timeout=$(get_config "watchdog.heartbeat_timeout" "120")

    # Check if heartbeat directory exists
    if [[ ! -d "$heartbeat_dir" ]]; then
        _needle_debug "knot: no heartbeat directory - worker $worker_name considered inactive"
        return 1
    fi

    # Look for a heartbeat file matching this worker
    # Heartbeat files are named <session>.json, and the worker field inside
    # matches the session. Try exact file match first, then scan files.
    local hb_file="$heartbeat_dir/${worker_name}.json"
    local found_match=false
    local last_heartbeat=""

    if [[ -f "$hb_file" ]]; then
        found_match=true
        last_heartbeat=$(jq -r '.last_heartbeat // ""' "$hb_file" 2>/dev/null)
    else
        # Scan heartbeat files for matching worker field
        for f in "$heartbeat_dir"/*.json; do
            [[ ! -f "$f" ]] && continue
            local w
            w=$(jq -r '.worker // ""' "$f" 2>/dev/null)
            if [[ "$w" == "$worker_name" ]]; then
                found_match=true
                last_heartbeat=$(jq -r '.last_heartbeat // ""' "$f" 2>/dev/null)
                break
            fi
        done
    fi

    if [[ "$found_match" != "true" ]] || [[ -z "$last_heartbeat" ]]; then
        _needle_debug "knot: no heartbeat found for worker $worker_name - considered inactive"
        return 1
    fi

    # Check if heartbeat is fresh (within timeout)
    local now last_ts beat_age
    now=$(date +%s)
    last_ts=$(date -d "$last_heartbeat" +%s 2>/dev/null || echo 0)

    if [[ "$last_ts" -eq 0 ]]; then
        _needle_debug "knot: cannot parse heartbeat timestamp for worker $worker_name - considered inactive"
        return 1
    fi

    beat_age=$((now - last_ts))

    if ((beat_age <= timeout)); then
        _needle_debug "knot: worker $worker_name is active (heartbeat ${beat_age}s old, timeout ${timeout}s)"
        return 0
    else
        _needle_debug "knot: worker $worker_name is stale (heartbeat ${beat_age}s old, timeout ${timeout}s)"
        return 1
    fi
}

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
