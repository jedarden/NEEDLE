#!/usr/bin/env bash
# NEEDLE Bead Claiming Module
# Atomic bead claiming with retry logic for multi-worker environments
#
# This module provides:
# - Atomic claiming via `br update --claim`
# - Weighted selection respecting priorities (P0=10x, P1=5x, P2=2x, P3=1x)
# - Retry logic when claim fails due to race conditions
# - Bead release functionality

# Source dependencies (if not already loaded)
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

if [[ -z "${_NEEDLE_CONSTANTS_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/constants.sh"
fi

if [[ -z "${_NEEDLE_JSON_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/json.sh"
fi

# Source telemetry for events
if [[ -z "${_NEEDLE_TELEMETRY_EVENTS_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../telemetry/events.sh"
fi

# Source diagnostic module for logging
if [[ -z "${_NEEDLE_DIAGNOSTIC_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/diagnostic.sh"
fi

# Source select module for _needle_get_claimable_beads fallback
if [[ -z "${_NEEDLE_SELECT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/select.sh"
fi

# Source intent module for proactive file reservation
if [[ -z "${_NEEDLE_INTENT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/intent.sh"
fi

# ============================================================================
# Claim-specific Priority Weights
# ============================================================================
# These weights are specific to the claim module
# P0 (critical) = 10x, P1 (high) = 5x, P2 (normal) = 2x, P3+ (low) = 1x
NEEDLE_CLAIM_PRIORITY_WEIGHTS=(
    10  # P0 - critical (10x more likely)
    5   # P1 - high (5x more likely)
    2   # P2 - normal (2x more likely)
    1   # P3 - low
    1   # P4+ - backlog (same as P3)
)

# Default retry configuration
NEEDLE_CLAIM_MAX_RETRIES="${NEEDLE_CLAIM_MAX_RETRIES:-5}"

# ============================================================================
# Weighted Selection Functions
# ============================================================================

# Get weight for a given priority level
# Usage: _needle_claim_get_weight <priority>
# Returns: weight multiplier (1-10)
_needle_claim_get_weight() {
    local priority="${1:-2}"  # Default to P2 (normal)

    # Validate priority is a number
    if ! [[ "$priority" =~ ^[0-9]+$ ]]; then
        priority=2
    fi

    # Cap at max defined priority (P4+ all get weight 1)
    if [[ $priority -ge ${#NEEDLE_CLAIM_PRIORITY_WEIGHTS[@]} ]]; then
        priority=$(( ${#NEEDLE_CLAIM_PRIORITY_WEIGHTS[@]} - 1 ))
    fi

    echo "${NEEDLE_CLAIM_PRIORITY_WEIGHTS[$priority]}"
}

# Select a bead from the ready queue using weighted random selection
# Usage: _needle_select_bead [--workspace <workspace>] [--json]
# Returns: bead ID (or full JSON object if --json specified)
# Exit codes:
#   0 - Success, bead selected
#   1 - No beads available or error
#
# Weight distribution:
#   P0 (critical): 10x weight - ~10x more likely to be selected
#   P1 (high):     5x weight  - ~5x more likely to be selected
#   P2 (normal):   2x weight  - ~2x more likely to be selected
#   P3+ (low):     1x weight  - base probability
#
# Example:
#   bead_id=$(_needle_select_bead --workspace /home/coder/NEEDLE)
#   bead_json=$(_needle_select_bead --workspace /home/coder/NEEDLE --json)
_needle_select_bead() {
    local workspace=""
    local output_json=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --workspace)
                workspace="$2"
                shift 2
                ;;
            --json)
                output_json=true
                shift
                ;;
            *)
                shift
                ;;
        esac
    done

    # Get claimable beads from br CLI (with fallback for br ready bug)
    local candidates
    candidates=$(_needle_get_claimable_beads ${workspace:+--workspace "$workspace"})

    # Handle empty or invalid response
    if [[ -z "$candidates" ]] || [[ "$candidates" == "[]" ]] || [[ "$candidates" == "null" ]]; then
        _needle_debug "No claimable beads available"
        return 1
    fi

    # Single jq call: validate, extract id and priority as tab-separated
    local bead_data
    bead_data=$(echo "$candidates" | jq -r '.[]? | select(.id != null and .id != "") | [.id, (.priority // 2)] | @tsv' 2>/dev/null)

    if [[ -z "$bead_data" ]]; then
        _needle_debug "No valid beads in response"
        return 1
    fi

    # Build weighted arrays using inline weight lookup (no subprocess per bead)
    local beads=()
    local weights=()
    local total_weight=0

    while IFS=$'\t' read -r id priority; do
        [[ -z "$id" ]] && continue

        # Inline weight calculation - avoids subprocess call per bead
        local weight
        if ! [[ "$priority" =~ ^[0-9]+$ ]]; then priority=2; fi
        case "$priority" in
            0) weight=10 ;; 1) weight=5 ;; 2) weight=2 ;; *) weight=1 ;;
        esac

        beads+=("$id")
        weights+=("$weight")
        total_weight=$((total_weight + weight))
    done <<< "$bead_data"

    # Check if we have any weighted entries
    if [[ ${#beads[@]} -eq 0 ]]; then
        _needle_debug "No valid beads after weighting"
        return 1
    fi

    # Weighted random selection using cumulative distribution
    # RANDOM is 0-32767, we scale to total_weight
    local rand=$((RANDOM % total_weight))
    local cumulative=0
    local selected_idx=0

    for i in "${!beads[@]}"; do
        cumulative=$((cumulative + weights[$i]))
        if ((rand < cumulative)); then
            selected_idx=$i
            break
        fi
    done

    local selected_bead_id="${beads[$selected_idx]}"
    _needle_debug "Selected bead: $selected_bead_id (from $total_weight weighted entries)"

    # Get full bead JSON if --json was specified
    if [[ "$output_json" == "true" ]]; then
        local selected_bead_json
        selected_bead_json=$(echo "$candidates" | jq -c --arg id "$selected_bead_id" '.[] | select(.id == $id)')
        if [[ -n "$selected_bead_json" ]]; then
            echo "$selected_bead_json"
            return 0
        else
            # Fallback to just the ID if JSON extraction fails
            _needle_warn "Could not extract full JSON for bead $selected_bead_id"
            echo "$selected_bead_id"
            return 0
        fi
    fi

    echo "$selected_bead_id"
    return 0
}

# ============================================================================
# Atomic Claim Functions
# ============================================================================

# Atomically claim a bead with retry logic
# Usage: _needle_claim_bead [--workspace <workspace>] --actor <actor> [--max-retries <n>]
# Returns: bead ID on success
# Exit codes:
#   0 - Success, bead claimed
#   1 - No beads available or all retries exhausted
#
# The retry logic handles race conditions where multiple workers attempt
# to claim the same bead simultaneously. SQLite's transaction isolation
# in `br` ensures only one worker succeeds - others get exit code 4.
#
# Example:
#   bead_id=$(_needle_claim_bead --workspace /home/coder/NEEDLE --actor worker-alpha)
_needle_claim_bead() {
    local workspace=""
    local actor=""
    local max_retries="${NEEDLE_CLAIM_MAX_RETRIES:-5}"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --workspace)
                workspace="$2"
                shift 2
                ;;
            --actor)
                actor="$2"
                shift 2
                ;;
            --max-retries)
                max_retries="$2"
                shift 2
                ;;
            *)
                shift
                ;;
        esac
    done

    # DIAGNOSTIC: Log claim attempt start
    _needle_diag_claim "Claim attempt started" \
        "workspace=$workspace" \
        "actor=$actor" \
        "max_retries=$max_retries" \
        "session=${NEEDLE_SESSION:-unknown}"

    # Validate required parameters
    if [[ -z "$actor" ]]; then
        _needle_error "claim_bead requires --actor parameter"
        _needle_diag_claim "Claim failed - missing actor parameter"
        return 1
    fi

    # Attempt to claim with retries
    local attempt=1
    while [[ $attempt -le $max_retries ]]; do
        # Select a bead using weighted random selection
        local bead_id
        bead_id=$(_needle_select_bead --workspace "$workspace")

        # DIAGNOSTIC: Log selection result
        _needle_diag_claim "Bead selection result" \
            "attempt=$attempt" \
            "bead_id=${bead_id:-<none>}" \
            "workspace=$workspace"

        if [[ -z "$bead_id" ]]; then
            # No beads available - this is normal, not an error
            _needle_debug "No beads available to claim"
            _needle_diag_claim "No beads available" \
                "attempt=$attempt" \
                "max_retries=$max_retries" \
                "workspace=$workspace"
            return 1
        fi

        _needle_debug "Attempting to claim bead $bead_id (attempt $attempt/$max_retries)"

        # Run pre_claim hook before attempting claim
        # exit 2 (abort) = return 1 from _needle_run_hook: stop claiming
        # exit 3 (skip)  = return 2 from _needle_run_hook: skip this bead, try another
        if declare -f _needle_run_hook &>/dev/null; then
            local pre_claim_result
            _needle_run_hook "pre_claim" "$bead_id" >&2
            pre_claim_result=$?
            if [[ $pre_claim_result -eq 1 ]]; then
                # Hook aborted claiming entirely
                _needle_debug "pre_claim hook aborted claim for bead $bead_id"
                return 1
            elif [[ $pre_claim_result -eq 2 ]]; then
                # Hook requested skip - try a different bead
                _needle_debug "pre_claim hook skipped bead $bead_id, retrying"
                attempt=$((attempt + 1))
                continue
            fi
        fi

        # Attempt atomic claim via br update --claim
        # br returns exit 0 on success, exit 4 on race condition (already claimed)
        # FIX: Run in workspace directory if provided (br operates on current directory)
        local claim_result
        if [[ -n "$workspace" && -d "$workspace" ]]; then
            claim_result=$(cd "$workspace" && br update "$bead_id" --claim --actor "$actor" 2>&1)
        else
            claim_result=$(br update "$bead_id" --claim --actor "$actor" 2>&1)
        fi
        local claim_exit=$?

        # DIAGNOSTIC: Log br call result
        _needle_diag_claim "br claim call completed" \
            "bead_id=$bead_id" \
            "attempt=$attempt" \
            "exit_code=$claim_exit" \
            "result_preview=${claim_result:0:100}"

        if [[ $claim_exit -eq 0 ]]; then
            # Success! Emit telemetry
            _needle_event_bead_claimed "$bead_id" \
                "actor=$actor" \
                "attempt=$attempt" \
                "workspace=$workspace"

            _needle_diag_claim "Bead claimed successfully" \
                "bead_id=$bead_id" \
                "actor=$actor" \
                "attempt=$attempt" \
                "workspace=$workspace"

            # NOTE: Redirect to stderr - stdout reserved for return value
            _needle_success "Claimed bead: $bead_id" >&2

            # Run post_claim hook after successful claim
            if declare -f _needle_run_hook &>/dev/null; then
                _needle_run_hook "post_claim" "$bead_id" >&2 || true
            fi

            # Attempt intent-based file reservation
            # This returns:
            #   0 - All files reserved successfully
            #   1 - Conflict detected (bead already released, dependency added)
            #   2 - No files declared (proceed normally)
            if declare -f _needle_claim_with_intent &>/dev/null; then
                local intent_result
                _needle_claim_with_intent "$bead_id" ${workspace:+--workspace "$workspace"} --actor "$actor"
                intent_result=$?

                if [[ $intent_result -eq 1 ]]; then
                    # Conflict detected - bead was released and dependency added
                    # Return special code to trigger retry with different bead
                    _needle_warn "File conflict detected for $bead_id, dependency added, trying another bead"
                    attempt=$((attempt + 1))
                    continue
                elif [[ $intent_result -eq 0 ]]; then
                    _needle_success "Intent-based reservation successful for $bead_id" >&2
                fi
                # intent_result == 2: no files to reserve, continue normally
            fi

            echo "$bead_id"
            return 0
        fi

        # Claim failed (race condition - another worker got it first)
        # Emit retry event and try again with a different bead
        _needle_telemetry_emit "bead.claim_retry" "warn" \
            "bead_id=$bead_id" \
            "attempt=$attempt" \
            "max_retries=$max_retries" \
            "actor=$actor"

        _needle_diag_claim "Claim race condition, retrying" \
            "bead_id=$bead_id" \
            "attempt=$attempt" \
            "exit_code=$claim_exit"

        _needle_verbose "Claim race condition for bead $bead_id, retrying..."

        attempt=$((attempt + 1))
    done

    # All retries exhausted
    _needle_warn "Failed to claim bead after $max_retries attempts"

    _needle_diag_claim "All claim retries exhausted" \
        "max_retries=$max_retries" \
        "actor=$actor" \
        "workspace=$workspace"

    _needle_diag_starvation "claim_retries_exhausted" \
        "max_retries=$max_retries" \
        "workspace=$workspace"

    _needle_telemetry_emit "bead.claim_exhausted" "error" \
        "max_retries=$max_retries" \
        "actor=$actor" \
        "workspace=$workspace"

    return 1
}

# ============================================================================
# Bead Release Functions
# ============================================================================

# Release a claimed bead back to the queue
# Usage: _needle_release_bead <bead_id> [--reason <reason>] [--actor <actor>]
# Exit codes:
#   0 - Success, bead released
#   1 - Failed to release bead
#
# Example:
#   _needle_release_bead nd-123 --reason "blocked by dependency" --actor worker-alpha
#
# NOTE: Uses SQL fallback because br CLI has CHECK constraint bug that prevents
# setting status='open' while claimed_by is set. The SQL directly clears all
# claim fields atomically.
_needle_release_bead() {
    local bead_id=""
    local reason="released"
    local actor="${NEEDLE_SESSION:-unknown}"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --reason)
                reason="$2"
                shift 2
                ;;
            --actor)
                actor="$2"
                shift 2
                ;;
            -*)
                # Skip unknown flags
                shift
                ;;
            *)
                # First positional argument is bead_id
                if [[ -z "$bead_id" ]]; then
                    bead_id="$1"
                fi
                shift
                ;;
        esac
    done

    # Validate required parameters
    if [[ -z "$bead_id" ]]; then
        _needle_error "release_bead requires bead_id parameter"
        return 1
    fi

    _needle_debug "Releasing bead $bead_id: $reason"

    # Determine workspace database path
    local db_path="${NEEDLE_WORKSPACE:-$(pwd)}/.beads/beads.db"

    # Try SQL-based release first (works around br CLI CHECK constraint bug)
    # Method 1: sqlite3 CLI
    if command -v sqlite3 &>/dev/null && [[ -f "$db_path" ]]; then
        local sql_result
        sql_result=$(sqlite3 "$db_path" \
            "UPDATE issues SET
                status = 'open',
                assignee = NULL,
                claimed_by = NULL,
                claim_timestamp = NULL
             WHERE id = '$bead_id' AND status = 'in_progress';
             SELECT changes();" 2>&1)

        if [[ "$sql_result" =~ ^[1-9][0-9]*$ ]] || [[ "$sql_result" == "1" ]]; then
            # Emit telemetry
            _needle_event_bead_released "$bead_id" \
                "reason=$reason" \
                "actor=$actor" \
                "method=sql_sqlite3"

            _needle_info "Released bead: $bead_id ($reason) via sqlite3"
            return 0
        else
            _needle_debug "sqlite3 release returned: $sql_result (may already be released)"
        fi
    fi

    # Method 2: Python sqlite3 (fallback when sqlite3 CLI not installed)
    if command -v python3 &>/dev/null && [[ -f "$db_path" ]]; then
        local py_result
        py_result=$(python3 -c "
import sqlite3
conn = sqlite3.connect('$db_path')
c = conn.cursor()
c.execute('''UPDATE issues SET
    status='open',
    assignee=NULL,
    claimed_by=NULL,
    claim_timestamp=NULL
    WHERE id='$bead_id' AND status='in_progress' ''')
conn.commit()
print(c.rowcount)
" 2>&1)

        if [[ "$py_result" == "1" ]]; then
            # Emit telemetry
            _needle_event_bead_released "$bead_id" \
                "reason=$reason" \
                "actor=$actor" \
                "method=sql_python"

            _needle_info "Released bead: $bead_id ($reason) via python3"
            return 0
        else
            _needle_debug "python3 release returned: $py_result (may already be released)"
        fi
    fi

    # Fallback: Try br CLI (may fail due to CHECK constraint bug)
    if br update "$bead_id" --status open 2>/dev/null; then
        # Emit telemetry
        _needle_event_bead_released "$bead_id" \
            "reason=$reason" \
            "actor=$actor" \
            "method=br_cli"

        _needle_info "Released bead: $bead_id ($reason) via br CLI"
        return 0
    fi

    # If both methods failed, check if bead is already released
    local current_status
    current_status=$(br show "$bead_id" --json 2>/dev/null | jq -r '.status // "unknown"')

    if [[ "$current_status" != "in_progress" ]]; then
        _needle_debug "Bead $bead_id already released (status=$current_status)"
        return 0
    fi

    _needle_warn "Failed to release bead $bead_id (status=$current_status)"
    return 1
}

# ============================================================================
# Claim Status Functions
# ============================================================================

# Check if a bead is currently claimed
# Usage: _needle_bead_is_claimed <bead_id>
# Returns: 0 if claimed, 1 if not claimed
_needle_bead_is_claimed() {
    local bead_id="$1"

    if [[ -z "$bead_id" ]]; then
        return 1
    fi

    # Query bead status via br
    local bead_json
    bead_json=$(br show "$bead_id" --json 2>/dev/null)

    if [[ -z "$bead_json" ]]; then
        return 1
    fi

    # Check if bead has an assignee
    local assignee
    assignee=$(echo "$bead_json" | jq -r '.assignee // empty')

    if [[ -n "$assignee" ]]; then
        return 0  # Claimed
    else
        return 1  # Not claimed
    fi
}

# Get the current assignee of a bead
# Usage: _needle_bead_assignee <bead_id>
# Returns: assignee name or empty string if unassigned
_needle_bead_assignee() {
    local bead_id="$1"

    if [[ -z "$bead_id" ]]; then
        return 1
    fi

    local bead_json
    bead_json=$(br show "$bead_id" --json 2>/dev/null)

    if [[ -z "$bead_json" ]]; then
        return 1
    fi

    echo "$bead_json" | jq -r '.assignee // empty'
}

# ============================================================================
# Statistics Functions
# ============================================================================

# Get statistics about the claimable bead pool
# Usage: _needle_claim_stats [--workspace <workspace>]
# Returns: JSON object with claim statistics
_needle_claim_stats() {
    local workspace=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --workspace)
                workspace="$2"
                shift 2
                ;;
            *)
                shift
                ;;
        esac
    done

    # Get claimable beads (with fallback for br ready bug)
    local candidates
    candidates=$(_needle_get_claimable_beads ${workspace:+--workspace "$workspace"})

    if [[ -z "$candidates" ]] || [[ "$candidates" == "[]" ]] || [[ "$candidates" == "null" ]]; then
        echo '{"total_beads":0,"weighted_pool_size":0,"by_priority":{}}'
        return 0
    fi

    # Single jq call to compute all statistics
    echo "$candidates" | jq -c '
        def get_weight: if . == 0 then 10 elif . == 1 then 5 elif . == 2 then 2 else 1 end;
        length as $total |
        [.[] | .priority // 2] |
        group_by(.) |
        map({
            priority: .[0],
            count: length,
            weight: (.[0] | get_weight),
            total_weight: (length * (.[0] | get_weight))
        }) |
        {
            total_beads: $total,
            weighted_pool_size: (map(.total_weight) | add // 0),
            by_priority: (map({("P\(.priority)"): {count: .count, weight: .weight}}) | add // {})
        }
    '
}

# ============================================================================
# Bead Creation with Default Unassignment
# ============================================================================

# Check if unassigned_by_default is enabled
# Usage: _needle_unassigned_by_default
# Returns: 0 if enabled, 1 if disabled
_needle_unassigned_by_default() {
    local enabled
    enabled=$(get_config "select.unassigned_by_default" "true" 2>/dev/null)

    case "$enabled" in
        true|True|TRUE|yes|Yes|YES|1) return 0 ;;
        *) return 1 ;;
    esac
}

# Create a bead and immediately release assignment if unassigned_by_default is enabled
# This prevents worker starvation by ensuring new beads are immediately claimable.
#
# Usage: _needle_create_bead [options] -- <title>
# All options are passed through to br create
# Returns: bead ID on success
# Exit codes:
#   0 - Success, bead created
#   1 - Failed to create bead
#
# The function:
# 1. Creates the bead using br create (which auto-assigns to creator)
# 2. If unassigned_by_default is enabled AND bead is not human-type, releases the assignment
# 3. Returns the bead ID
#
# Example:
#   bead_id=$(_needle_create_bead --type task --priority 1 -- "Fix the bug")
#   bead_id=$(_needle_create_bead --type human --title "HUMAN: Choose option")
#
# NOTE: Human-type beads are always kept assigned (human needs to see them).
# Use --assignee to explicitly keep assignment for other types.
_needle_create_bead() {
    local br_args=()
    local title=""
    local has_assignee=false
    local is_human_type=false
    local workspace="${NEEDLE_WORKSPACE:-$(pwd)}"

    # Parse arguments, collecting br create args
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --workspace)
                workspace="$2"
                shift 2
                ;;
            --assignee|-a)
                has_assignee=true
                br_args+=("$1" "$2")
                shift 2
                ;;
            --type|-t)
                br_args+=("$1" "$2")
                if [[ "$2" == "human" ]]; then
                    is_human_type=true
                fi
                shift 2
                ;;
            --title)
                shift
                br_args+=("--title" "$1")
                title="$1"
                shift
                ;;
            --description|-d|--body)
                br_args+=("$1" "$2")
                shift 2
                ;;
            --labels|-l)
                br_args+=("$1" "$2")
                shift 2
                ;;
            --label)
                # Single label - convert to --labels
                br_args+=("--labels" "$2")
                shift 2
                ;;
            --priority|-p)
                br_args+=("$1" "$2")
                shift 2
                ;;
            --parent)
                br_args+=("$1" "$2")
                shift 2
                ;;
            --silent|--json|--dry-run)
                br_args+=("$1")
                shift
                ;;
            --)
                # Title separator
                shift
                title="$*"
                break
                ;;
            *)
                br_args+=("$1")
                shift
                ;;
        esac
    done

    # If we have remaining args after --, that's the title
    if [[ -z "$title" ]] && [[ $# -gt 0 ]]; then
        title="$*"
        br_args+=("--title" "$title")
    fi

    # Create the bead
    local create_output
    local create_exit

    _needle_debug "Creating bead: ${title:-<no title>}"
    _needle_debug "br create args: ${br_args[*]}"

    if [[ -n "$workspace" && -d "$workspace" ]]; then
        create_output=$(cd "$workspace" && br create "${br_args[@]}" 2>&1)
    else
        create_output=$(br create "${br_args[@]}" 2>&1)
    fi
    create_exit=$?

    if [[ $create_exit -ne 0 ]]; then
        _needle_error "Failed to create bead: $create_output"
        return 1
    fi

    # Extract bead ID from output
    # Format: "Created issue nd-xxxxx" or just the ID
    local bead_id
    bead_id=$(echo "$create_output" | grep -oP '(?:Created issue\s+)?[a-z]{2,}-[a-z0-9]+' | head -1)

    if [[ -z "$bead_id" ]]; then
        _needle_warn "Could not extract bead ID from: $create_output"
        # Try to get last word as ID
        bead_id=$(echo "$create_output" | awk '{print $NF}')
    fi

    _needle_debug "Created bead: $bead_id"

    # Check if we should release the assignment
    # Skip release if:
    # 1. unassigned_by_default is disabled
    # 2. An explicit assignee was provided (human wants to keep it)
    # 3. This is a human-type bead (alerts that humans need to see)
    if ! _needle_unassigned_by_default; then
        _needle_debug "unassigned_by_default is disabled, keeping assignment"
        echo "$bead_id"
        return 0
    fi

    if $has_assignee; then
        _needle_debug "Explicit assignee provided, keeping assignment"
        echo "$bead_id"
        return 0
    fi

    if $is_human_type; then
        _needle_debug "Human-type bead, keeping assignment for visibility"
        echo "$bead_id"
        return 0
    fi

    # Release the assignment so workers can claim it immediately
    _needle_debug "Releasing assignment for bead $bead_id (unassigned_by_default enabled)"

    local release_result
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        release_result=$(cd "$workspace" && br update "$bead_id" --assignee "" --status open 2>&1)
    else
        release_result=$(br update "$bead_id" --assignee "" --status open 2>&1)
    fi
    local release_exit=$?

    if [[ $release_exit -eq 0 ]]; then
        _needle_debug "Released assignment for bead $bead_id"
    else
        # Non-fatal - bead was created, just couldn't release
        _needle_warn "Created bead $bead_id but failed to release assignment: $release_result"
    fi

    echo "$bead_id"
    return 0
}

# ============================================================================
# Direct Execution Support (for testing)
# ============================================================================
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-}" in
        select)
            shift
            _needle_select_bead "$@"
            ;;
        claim)
            shift
            _needle_claim_bead "$@"
            ;;
        release)
            shift
            _needle_release_bead "$@"
            ;;
        stats)
            shift
            _needle_claim_stats "$@" | jq .
            ;;
        is-claimed)
            shift
            if _needle_bead_is_claimed "$1"; then
                echo "claimed"
            else
                echo "unclaimed"
            fi
            ;;
        assignee)
            shift
            _needle_bead_assignee "$1"
            ;;
        -h|--help)
            echo "Usage: $0 <command> [options]"
            echo ""
            echo "Commands:"
            echo "  select [--workspace <ws>] [--json]   Select a bead using weighted selection"
            echo "  claim --actor <name> [options]       Atomically claim a bead with retry"
            echo "  release <bead_id> [--reason <r>]     Release a claimed bead"
            echo "  stats [--workspace <ws>]             Show claim pool statistics"
            echo "  is-claimed <bead_id>                 Check if bead is claimed"
            echo "  assignee <bead_id>                   Get bead assignee"
            echo ""
            echo "Weight Configuration:"
            echo "  P0 (critical): 10x weight"
            echo "  P1 (high):     5x weight"
            echo "  P2 (normal):   2x weight"
            echo "  P3+ (low):     1x weight"
            echo ""
            echo "Environment Variables:"
            echo "  NEEDLE_CLAIM_MAX_RETRIES  Max claim attempts (default: 5)"
            ;;
        *)
            echo "Unknown command: $1" >&2
            echo "Use --help for usage information" >&2
            exit 1
            ;;
    esac
fi
