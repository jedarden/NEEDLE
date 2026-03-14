#!/usr/bin/env bash
# NEEDLE Strand: mend (Priority 3)
# Maintenance and cleanup tasks
#
# Implementation: nd-1sk
#
# This strand handles maintenance tasks such as:
# - Cleaning up orphaned claims (beads assigned to dead workers)
# - Pruning old heartbeat files from dead workers
# - Log rotation and cleanup
#
# Usage:
#   _needle_strand_mend <workspace> <agent>
#
# Return values:
#   0 - Work was found and processed
#   1 - No work found (fallthrough to next strand)

# Source diagnostic module if not already loaded
if [[ -z "${_NEEDLE_DIAGNOSTIC_LOADED:-}" ]]; then
    NEEDLE_SRC="${NEEDLE_SRC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
    source "$NEEDLE_SRC/lib/diagnostic.sh"
fi

# ============================================================================
# Main Strand Entry Point
# ============================================================================

_needle_strand_mend() {
    local workspace="$1"
    local agent="$2"

    _needle_diag_strand "mend" "Mend strand started" \
        "workspace=$workspace" \
        "agent=$agent" \
        "session=${NEEDLE_SESSION:-unknown}"

    _needle_debug "mend strand: checking for maintenance tasks across all workspaces"

    local work_done=false
    local orphaned_count=0
    local stale_count=0
    local heartbeat_count=0
    local log_count=0

    # Collect all configured workspaces to check for orphaned/stale claims
    local workspaces=()
    workspaces+=("$workspace")

    # Add all configured workspaces from config
    local config
    config=$(load_config 2>/dev/null)
    if [[ -n "$config" ]]; then
        while IFS= read -r ws; do
            [[ -z "$ws" ]] && continue
            # Deduplicate — skip if already in list
            local already=false
            for existing in "${workspaces[@]}"; do
                if [[ "$existing" == "$ws" ]]; then
                    already=true
                    break
                fi
            done
            if ! $already && [[ -d "$ws/.beads" ]]; then
                workspaces+=("$ws")
            fi
        done < <(echo "$config" | jq -r '.workspaces[]? // empty' 2>/dev/null)
    fi

    # 1. Clean orphaned claims across all workspaces
    for ws in "${workspaces[@]}"; do
        _needle_diag_strand "mend" "Checking for orphaned claims" "workspace=$ws"
        if _needle_mend_orphaned_claims "$ws"; then
            work_done=true
            ((orphaned_count++))
        fi
    done

    # 2. Release stale claims across all workspaces
    for ws in "${workspaces[@]}"; do
        _needle_diag_strand "mend" "Checking for stale claims" "workspace=$ws"
        if _needle_mend_stale_claims "$ws"; then
            work_done=true
            ((stale_count++))
        fi
    done

    # 3. Prune old heartbeat files from dead workers
    _needle_diag_strand "mend" "Checking for old heartbeats"
    if _needle_mend_old_heartbeats; then
        work_done=true
        heartbeat_count=1
    fi

    # 4. Log rotation/cleanup (if configured)
    _needle_diag_strand "mend" "Checking for log cleanup"
    if _needle_mend_logs; then
        work_done=true
        log_count=1
    fi

    if $work_done; then
        _needle_diag_strand "mend" "Mend strand completed work" \
            "workspace=$workspace" \
            "orphaned_cleaned=$orphaned_count" \
            "stale_cleaned=$stale_count" \
            "heartbeats_cleaned=$heartbeat_count" \
            "logs_cleaned=$log_count"

        _needle_debug "mend strand: maintenance completed"
        return 0
    fi

    _needle_diag_strand "mend" "Mend strand found no work" \
        "workspace=$workspace" \
        "orphaned_checked=true" \
        "stale_checked=true" \
        "heartbeats_checked=true" \
        "logs_checked=true"

    _needle_debug "mend strand: no maintenance needed"
    return 1  # No work found
}

# ============================================================================
# Bead Release Helper
# ============================================================================

# Release a bead directly via sqlite3 using a known db_path
# Usage: _needle_mend_release_bead <db_path> <bead_id> <reason>
# Returns: 0 on success, 1 on failure
_needle_mend_release_bead() {
    local db_path="$1"
    local bead_id="$2"
    local reason="${3:-released}"

    if [[ ! -f "$db_path" ]]; then
        _needle_warn "mend: database not found at $db_path"
        return 1
    fi

    # Use br update with --db to release the bead
    if br update "$bead_id" --status open --assignee "" --db="$db_path" --lock-timeout 5000 2>/dev/null; then
        _needle_info "Released bead: $bead_id ($reason) via br update"
        return 0
    fi

    # Fallback: try sqlite3 if br update fails (CHECK constraint bug)
    if command -v sqlite3 &>/dev/null; then
        local sql_result
        sql_result=$(sqlite3 "$db_path" \
            "UPDATE issues SET
                status = 'open',
                assignee = NULL,
                claimed_by = NULL,
                claim_timestamp = NULL
             WHERE id = '$bead_id' AND status = 'in_progress';
             SELECT changes();" 2>&1)

        if [[ "$sql_result" == "1" ]] || [[ "$sql_result" =~ ^[1-9][0-9]*$ ]]; then
            _needle_info "Released bead: $bead_id ($reason) via sqlite3"
            return 0
        fi
    fi

    _needle_warn "Failed to release bead: $bead_id"
    return 1
}

# ============================================================================
# Orphaned Claims Cleanup
# ============================================================================

# Detect and release orphaned claims (beads assigned to dead workers)
# An orphaned claim is a bead with an assignee whose worker heartbeat is missing
#
# Usage: _needle_mend_orphaned_claims <workspace>
# Returns: 0 if any claims were released, 1 if none
_needle_mend_orphaned_claims() {
    local workspace="$1"
    local fixed=0

    # Get heartbeat directory path
    local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"

    # Ensure heartbeat directory exists
    if [[ ! -d "$heartbeat_dir" ]]; then
        _needle_debug "mend: no heartbeat directory found"
        return 1
    fi

    # Resolve the workspace database path
    local db_path=""
    if [[ -f "$workspace/.beads/beads.db" ]]; then
        db_path="$workspace/.beads/beads.db"
    else
        # Try to find .beads in subdirectories (workspace may be a parent)
        local found_db
        found_db=$(find "$workspace" -maxdepth 2 -path '*/.beads/beads.db' -type f 2>/dev/null | head -1)
        if [[ -n "$found_db" ]]; then
            db_path="$found_db"
        fi
    fi

    if [[ -z "$db_path" ]]; then
        _needle_debug "mend: no beads database found in $workspace"
        return 1
    fi

    # Get all in-progress beads from the workspace
    local in_progress
    in_progress=$(br list --db="$db_path" --status in_progress --json 2>/dev/null)

    # Handle empty or null results
    if [[ -z "$in_progress" ]] || [[ "$in_progress" == "[]" ]] || [[ "$in_progress" == "null" ]]; then
        _needle_debug "mend: no in-progress beads found in $workspace"
        return 1
    fi

    # Check if br command succeeded and returned valid JSON
    if ! echo "$in_progress" | jq -e '.[]' &>/dev/null; then
        _needle_debug "mend: no valid in-progress beads data"
        return 1
    fi

    # Iterate through in-progress beads
    while IFS= read -r bead; do
        local bead_id assignee

        # Extract bead ID and assignee
        bead_id=$(echo "$bead" | jq -r '.id // empty')
        assignee=$(echo "$bead" | jq -r '.assignee // empty')

        # Skip beads without an assignee
        if [[ -z "$assignee" ]]; then
            continue
        fi

        # Skip if we couldn't extract bead ID
        if [[ -z "$bead_id" ]]; then
            continue
        fi

        # Check if the assignee worker is still alive
        local heartbeat_file="$heartbeat_dir/${assignee}.json"

        if [[ ! -f "$heartbeat_file" ]]; then
            # No heartbeat file - worker is dead, this is an orphaned claim
            _needle_warn "Found orphaned claim: $bead_id (worker: $assignee - no heartbeat)"

            # Release the claim via SQL (br update --release doesn't exist)
            if _needle_mend_release_bead "$db_path" "$bead_id" "orphaned_claim"; then
                _needle_info "Released orphaned claim: $bead_id"

                # Emit event for monitoring
                _needle_emit_event "mend.orphan_released" \
                    "Released orphaned bead claim" \
                    "bead_id=$bead_id" \
                    "assignee=$assignee" \
                    "workspace=$workspace"

                ((fixed++))
            else
                _needle_warn "Failed to release orphaned claim: $bead_id"
            fi
        else
            # Heartbeat file exists - check if the process is actually alive
            local pid
            pid=$(jq -r '.pid // 0' "$heartbeat_file" 2>/dev/null)

            if [[ -n "$pid" ]] && [[ "$pid" != "0" ]] && [[ "$pid" != "null" ]]; then
                # Check if process is running
                if ! kill -0 "$pid" 2>/dev/null; then
                    # Process is dead but heartbeat file remains - orphaned claim
                    _needle_warn "Found orphaned claim: $bead_id (worker: $assignee - process $pid dead)"

                    # Release the claim via SQL
                    if _needle_mend_release_bead "$db_path" "$bead_id" "orphaned_claim"; then
                        _needle_info "Released orphaned claim: $bead_id"

                        # Emit event for monitoring
                        _needle_emit_event "mend.orphan_released" \
                            "Released orphaned bead claim" \
                            "bead_id=$bead_id" \
                            "assignee=$assignee" \
                            "workspace=$workspace" \
                            "dead_pid=$pid"

                        ((fixed++))
                    else
                        _needle_warn "Failed to release orphaned claim: $bead_id"
                    fi
                fi
            fi
        fi
    done < <(echo "$in_progress" | jq -c '.[]' 2>/dev/null)

    if ((fixed > 0)); then
        _needle_info "Released $fixed orphaned claim(s)"
        return 0
    fi

    return 1
}

# ============================================================================
# Stale Claims Detection
# ============================================================================

# Detect and release stale claims (beads held longer than threshold)
# A stale claim is a bead that has been in_progress for too long,
# regardless of whether the worker is still alive.
#
# This is different from orphaned claims:
# - Orphaned: Worker is dead (no heartbeat)
# - Stale: Claim has been held too long (time-based)
#
# Usage: _needle_mend_stale_claims <workspace>
# Returns: 0 if any claims were released, 1 if none
_needle_mend_stale_claims() {
    local workspace="$1"
    local released=0

    # Get stale claim threshold (default: 1 hour = 3600 seconds)
    local stale_threshold
    stale_threshold=$(get_config "mend.stale_claim_threshold" "3600")

    _needle_debug "mend: checking for stale claims (threshold: ${stale_threshold}s)"

    # Resolve the workspace database path
    local db_path=""
    if [[ -f "$workspace/.beads/beads.db" ]]; then
        db_path="$workspace/.beads/beads.db"
    else
        local found_db
        found_db=$(find "$workspace" -maxdepth 2 -path '*/.beads/beads.db' -type f 2>/dev/null | head -1)
        if [[ -n "$found_db" ]]; then
            db_path="$found_db"
        fi
    fi

    if [[ -z "$db_path" ]]; then
        _needle_debug "mend: no beads database found in $workspace"
        return 1
    fi

    # Get all in-progress beads from the workspace
    local in_progress
    in_progress=$(br list --db="$db_path" --status in_progress --json 2>/dev/null)

    # Handle empty or null results
    if [[ -z "$in_progress" ]] || [[ "$in_progress" == "[]" ]] || [[ "$in_progress" == "null" ]]; then
        _needle_debug "mend: no in-progress beads to check for staleness"
        return 1
    fi

    # Check if br command succeeded and returned valid JSON
    if ! echo "$in_progress" | jq -e '.[]' &>/dev/null; then
        _needle_debug "mend: no valid in-progress beads data for stale check"
        return 1
    fi

    local now
    now=$(date +%s)

    # Iterate through in-progress beads
    while IFS= read -r bead; do
        local bead_id assignee claim_timestamp updated_at claim_epoch age

        # Extract bead ID and timestamps
        bead_id=$(echo "$bead" | jq -r '.id // empty')
        assignee=$(echo "$bead" | jq -r '.assignee // .claimed_by // empty')
        claim_timestamp=$(echo "$bead" | jq -r '.claim_timestamp // empty')
        updated_at=$(echo "$bead" | jq -r '.updated_at // empty')

        # Skip beads without an ID
        if [[ -z "$bead_id" ]]; then
            continue
        fi

        # Determine the claim time - prefer claim_timestamp, fall back to updated_at
        local claim_time=""
        if [[ -n "$claim_timestamp" ]] && [[ "$claim_timestamp" != "null" ]]; then
            claim_time="$claim_timestamp"
        elif [[ -n "$updated_at" ]] && [[ "$updated_at" != "null" ]]; then
            claim_time="$updated_at"
        else
            # No timestamp available, skip this bead
            _needle_debug "mend: skipping $bead_id - no timestamp available"
            continue
        fi

        # Convert ISO8601 timestamp to epoch seconds
        # Handle various ISO8601 formats: 2026-03-03T10:09:09.969909498Z or 2026-03-03T10:09:09Z
        claim_epoch=$(_needle_parse_iso8601 "$claim_time")

        if [[ -z "$claim_epoch" ]] || [[ "$claim_epoch" == "0" ]]; then
            _needle_debug "mend: could not parse timestamp for $bead_id: $claim_time"
            continue
        fi

        # Calculate age of claim
        age=$((now - claim_epoch))

        _needle_verbose "mend: $bead_id age=${age}s (threshold=${stale_threshold}s)"

        # Check if claim is stale
        if ((age > stale_threshold)); then
            _needle_warn "Found stale claim: $bead_id (age: ${age}s, threshold: ${stale_threshold}s, assignee: ${assignee:-unknown})"

            # Release the stale claim using SQL fallback (works around br CLI CHECK constraint bug)
            if _needle_mend_release_bead "$db_path" "$bead_id" "stale_claim_auto_release"; then
                _needle_info "Released stale claim: $bead_id (held for ${age}s)"

                # Emit event for monitoring
                _needle_emit_event "mend.stale_released" \
                    "Released stale bead claim" \
                    "bead_id=$bead_id" \
                    "assignee=${assignee:-unknown}" \
                    "workspace=$workspace" \
                    "age_seconds=$age" \
                    "threshold_seconds=$stale_threshold"

                ((released++))
            else
                _needle_warn "Failed to release stale claim: $bead_id"
            fi
        fi
    done < <(echo "$in_progress" | jq -c '.[]' 2>/dev/null)

    if ((released > 0)); then
        _needle_info "Released $released stale claim(s)"
        return 0
    fi

    return 1
}

# Parse ISO8601 timestamp to epoch seconds
# Handles formats: 2026-03-03T10:09:09Z, 2026-03-03T10:09:09.123456Z, 2026-03-03T10:09:09+00:00
#
# Usage: _needle_parse_iso8601 <timestamp>
# Returns: epoch seconds, or 0 on failure
_needle_parse_iso8601() {
    local timestamp="$1"

    if [[ -z "$timestamp" ]] || [[ "$timestamp" == "null" ]]; then
        echo 0
        return 1
    fi

    local epoch=0

    # Try GNU date first (Linux)
    if epoch=$(date -d "$timestamp" +%s 2>/dev/null) && [[ -n "$epoch" ]] && [[ "$epoch" =~ ^[0-9]+$ ]]; then
        echo "$epoch"
        return 0
    fi

    # Try BSD date (macOS) - strip milliseconds first
    local stripped_ts="${timestamp%%.*}"
    if epoch=$(date -j -f "%Y-%m-%dT%H:%M:%S" "${stripped_ts}" +%s 2>/dev/null) && [[ -n "$epoch" ]] && [[ "$epoch" =~ ^[0-9]+$ ]]; then
        echo "$epoch"
        return 0
    fi

    # Fallback: Parse ISO8601 manually (YYYY-MM-DDTHH:MM:SS)
    if [[ "$timestamp" =~ ^([0-9]{4})-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2}) ]]; then
        local year="${BASH_REMATCH[1]}"
        local month="${BASH_REMATCH[2]}"
        local day="${BASH_REMATCH[3]}"
        local hour="${BASH_REMATCH[4]}"
        local minute="${BASH_REMATCH[5]}"
        local second="${BASH_REMATCH[6]}"

        # Use Python for reliable conversion if available
        if command -v python3 &>/dev/null; then
            epoch=$(python3 -c "
import datetime
try:
    dt = datetime.datetime(${year}, ${month}, ${day}, ${hour}, ${minute}, ${second})
    print(int(dt.timestamp()))
except:
    print(0)
" 2>/dev/null)
            if [[ -n "$epoch" ]] && [[ "$epoch" =~ ^[0-9]+$ ]] && [[ "$epoch" != "0" ]]; then
                echo "$epoch"
                return 0
            fi
        fi
    fi

    echo 0
    return 1
}

# ============================================================================
# Heartbeat Cleanup
# ============================================================================

# Clean up old heartbeat files from dead workers
# Removes heartbeat files where:
# 1. The process is no longer running, AND
# 2. The heartbeat is older than the configured max age
#
# Usage: _needle_mend_old_heartbeats
# Returns: 0 if any heartbeats were cleaned, 1 if none
_needle_mend_old_heartbeats() {
    local max_age
    max_age=$(get_config "mend.heartbeat_max_age" "3600")  # Default: 1 hour

    local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"

    # Ensure heartbeat directory exists
    if [[ ! -d "$heartbeat_dir" ]]; then
        _needle_debug "mend: no heartbeat directory to clean"
        return 1
    fi

    # Count heartbeat files
    local heartbeat_count
    heartbeat_count=$(find "$heartbeat_dir" -name "*.json" -type f 2>/dev/null | wc -l)

    if [[ "$heartbeat_count" -eq 0 ]]; then
        _needle_debug "mend: no heartbeat files to check"
        return 1
    fi

    local now
    now=$(date +%s)
    local cleaned=0

    # Iterate through heartbeat files
    for heartbeat_file in "$heartbeat_dir"/*.json; do
        [[ -f "$heartbeat_file" ]] || continue

        local pid last_heartbeat last_epoch age

        # Extract process ID and last heartbeat time
        pid=$(jq -r '.pid // 0' "$heartbeat_file" 2>/dev/null)
        last_heartbeat=$(jq -r '.last_heartbeat // .started // ""' "$heartbeat_file" 2>/dev/null)

        # Convert ISO8601 timestamp to epoch
        if [[ -n "$last_heartbeat" ]]; then
            # Try GNU date first, then BSD date
            last_epoch=$(date -d "$last_heartbeat" +%s 2>/dev/null || \
                         date -j -f "%Y-%m-%dT%H:%M:%S" "${last_heartbeat%%.*}" +%s 2>/dev/null || \
                         echo 0)
        else
            last_epoch=0
        fi

        # Calculate age of heartbeat
        if [[ "$last_epoch" -gt 0 ]]; then
            age=$((now - last_epoch))
        else
            # If we can't parse the timestamp, assume it's old
            age=$((max_age + 1))
        fi

        # Determine if we should clean up this heartbeat
        local should_cleanup=false

        # Check if process is dead
        local process_alive=false
        if [[ -n "$pid" ]] && [[ "$pid" != "0" ]] && [[ "$pid" != "null" ]]; then
            if kill -0 "$pid" 2>/dev/null; then
                process_alive=true
            fi
        fi

        # Cleanup if process is dead AND heartbeat is old enough
        if [[ "$process_alive" == "false" ]] && ((age > max_age)); then
            should_cleanup=true
        fi

        if $should_cleanup; then
            local worker_name
            worker_name=$(basename "$heartbeat_file" .json)

            if rm -f "$heartbeat_file"; then
                _needle_debug "Cleaned up stale heartbeat: $worker_name (age: ${age}s, pid: ${pid:-unknown})"

                # Emit event for monitoring
                _needle_emit_event "mend.heartbeat_cleaned" \
                    "Cleaned up stale worker heartbeat" \
                    "worker=$worker_name" \
                    "pid=${pid:-unknown}" \
                    "age_seconds=$age"

                ((cleaned++))
            else
                _needle_warn "Failed to clean up heartbeat: $heartbeat_file"
            fi
        fi
    done

    if ((cleaned > 0)); then
        _needle_info "Cleaned up $cleaned stale heartbeat file(s)"
        return 0
    fi

    return 1
}

# ============================================================================
# Log Cleanup
# ============================================================================

# Prune old log files based on configuration
# Removes oldest logs when count exceeds max_files
#
# Usage: _needle_mend_logs
# Returns: 0 if any logs were pruned, 1 if none
_needle_mend_logs() {
    local max_files
    max_files=$(get_config "mend.max_log_files" "100")  # Default: keep 100 log files

    local log_dir="$NEEDLE_HOME/$NEEDLE_LOG_DIR"

    # Ensure log directory exists
    if [[ ! -d "$log_dir" ]]; then
        _needle_debug "mend: no log directory to prune"
        return 1
    fi

    # Count log files
    local log_count
    log_count=$(find "$log_dir" -name "*.jsonl" -type f 2>/dev/null | wc -l)

    if [[ "$log_count" -le "$max_files" ]]; then
        _needle_debug "mend: log count ($log_count) within limit ($max_files)"
        return 1
    fi

    # Calculate how many files to delete
    local to_delete=$((log_count - max_files))

    if [[ "$to_delete" -le 0 ]]; then
        return 1
    fi

    _needle_debug "mend: pruning $to_delete old log file(s) (current: $log_count, max: $max_files)"

    # Find and delete the oldest log files
    local deleted=0
    while IFS= read -r file; do
        if [[ -n "$file" ]] && rm -f "$file"; then
            ((deleted++))
        fi
    done < <(find "$log_dir" -name "*.jsonl" -type f -printf '%T@ %p\n' 2>/dev/null | \
             sort -n | head -n "$to_delete" | cut -d' ' -f2-)

    if ((deleted > 0)); then
        _needle_info "Pruned $deleted old log file(s)"

        # Emit event for monitoring
        _needle_emit_event "mend.logs_pruned" \
            "Pruned old log files" \
            "count=$deleted" \
            "previous_count=$log_count" \
            "max_files=$max_files"

        return 0
    fi

    return 1
}

# ============================================================================
# Utility Functions
# ============================================================================

# Check if mend strand should run based on last run time
# Prevents excessive maintenance checks
#
# Usage: _needle_mend_should_run
# Returns: 0 if should run, 1 if skipped (too soon)
_needle_mend_should_run() {
    local min_interval
    min_interval=$(get_config "mend.min_interval" "60")  # Default: 60 seconds

    local state_file="$NEEDLE_HOME/$NEEDLE_STATE_DIR/mend_last_run"
    local now
    now=$(date +%s)

    # Check if state file exists
    if [[ -f "$state_file" ]]; then
        local last_run
        last_run=$(cat "$state_file" 2>/dev/null)

        if [[ -n "$last_run" ]] && [[ "$last_run" =~ ^[0-9]+$ ]]; then
            local elapsed=$((now - last_run))

            if [[ "$elapsed" -lt "$min_interval" ]]; then
                _needle_debug "mend: skipping (last run ${elapsed}s ago, min interval: ${min_interval}s)"
                return 1
            fi
        fi
    fi

    # Update last run time
    echo "$now" > "$state_file"
    return 0
}

# Get statistics about current maintenance state
#
# Usage: _needle_mend_stats
# Returns: JSON object with stats
_needle_mend_stats() {
    local heartbeat_dir="$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
    local log_dir="$NEEDLE_HOME/$NEEDLE_LOG_DIR"

    local heartbeat_count=0
    local log_count=0
    local log_size=0

    # Count heartbeat files
    if [[ -d "$heartbeat_dir" ]]; then
        heartbeat_count=$(find "$heartbeat_dir" -name "*.json" -type f 2>/dev/null | wc -l)
    fi

    # Count log files and total size
    if [[ -d "$log_dir" ]]; then
        log_count=$(find "$log_dir" -name "*.jsonl" -type f 2>/dev/null | wc -l)
        log_size=$(du -sb "$log_dir" 2>/dev/null | cut -f1 || echo 0)
    fi

    # Output as JSON
    _needle_json_object \
        "heartbeat_count=$heartbeat_count" \
        "log_count=$log_count" \
        "log_size_bytes=$log_size"
}
