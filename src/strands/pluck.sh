#!/usr/bin/env bash
# NEEDLE Strand: pluck (Priority 1)
# Primary work from configured workspaces
#
# Implementation: nd-2gc
#
# This strand searches for work in the configured primary workspaces.
# It is the highest priority strand and should be checked first.
#
# Flow:
#   1. Get configured workspaces (or use provided workspace)
#   2. For each workspace, attempt to claim a bead atomically
#   3. Run mitosis check on claimed bead
#   4. If mitosis triggered, split and release (fallthrough)
#   5. If atomic, build prompt and dispatch to agent
#   6. Capture output and exit code
#   7. Mark bead complete or failed
#   8. Emit telemetry events throughout
#
# Usage:
#   _needle_strand_pluck <workspace> <agent>
#
# Return values:
#   0 - Work was found and processed
#   1 - No work found (fallthrough to next strand)

# Source dependencies (if not already loaded)
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

if [[ -z "${_NEEDLE_CONFIG_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/config.sh"
fi

if [[ -z "${_NEEDLE_CONSTANTS_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/constants.sh"
fi

# Source diagnostic module for logging
if [[ -z "${_NEEDLE_DIAGNOSTIC_LOADED:-}" ]]; then
    NEEDLE_SRC="${NEEDLE_SRC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
    source "$NEEDLE_SRC/lib/diagnostic.sh"
fi

# Source bead modules
if [[ -z "${_NEEDLE_CLAIM_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../bead/claim.sh"
fi

if [[ -z "${_NEEDLE_MITOSIS_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../bead/mitosis.sh"
fi

if [[ -z "${_NEEDLE_PROMPT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../bead/prompt.sh"
fi

# Source verification module
if [[ -z "${_NEEDLE_VERIFY_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../bead/verify.sh"
fi

# Source agent modules
if [[ -z "${_NEEDLE_DISPATCH_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../agent/dispatch.sh"
fi

# Source telemetry
if [[ -z "${_NEEDLE_TELEMETRY_EVENTS_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../telemetry/events.sh"
fi

# Source effort module for per-bead cost attribution on close
if [[ -z "${_NEEDLE_EFFORT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../telemetry/effort.sh"
fi

# Source tokens module for per-bead token extraction
source "$(dirname "${BASH_SOURCE[0]}")/../telemetry/tokens.sh"

# ============================================================================
# Configuration Helpers
# ============================================================================

# Get the workspace this worker was deployed to.
# Pluck only works its assigned workspace — discovery is explore's job.
#
# Usage: _needle_pluck_get_workspaces [deployed_workspace]
# Returns: The single workspace path this worker should pluck from
_needle_pluck_get_workspaces() {
    local fallback="${1:-}"

    if [[ -n "$fallback" ]]; then
        local expanded="${fallback/#\~/$HOME}"
        echo "$expanded"
        return 0
    fi

    # No workspace provided — shouldn't happen, but be safe
    _needle_warn "pluck: no workspace configured"
    return 1
}

# Check if the pluck strand is enabled in config
#
# Reads strands.pluck from the YAML config file.
# Defaults to enabled when config is absent or the key is not set.
#
# Usage: _needle_pluck_is_enabled
# Returns: 0 if enabled, 1 if explicitly disabled
_needle_pluck_is_enabled() {
    local config_file="${NEEDLE_CONFIG_FILE:-}"

    if [[ -z "$config_file" ]] || [[ ! -f "$config_file" ]]; then
        return 0
    fi

    local pluck_value
    pluck_value=$(awk '/^strands:/{in_strands=1; next} /^[^ \t]/{in_strands=0} in_strands && /pluck:/{gsub(/.*pluck: */, ""); print; exit}' "$config_file" 2>/dev/null)

    if [[ "$pluck_value" == "false" ]]; then
        return 1
    fi
    return 0
}

# ============================================================================
# Per-Bead Failure Tracking
# ============================================================================

# Get the per-bead failure count from the bead's 'failure-count:N' label.
#
# Usage: _needle_get_bead_failure_count <bead_id> [workspace]
# Outputs: Integer failure count (0 if no label found)
_needle_get_bead_failure_count() {
    local bead_id="$1"
    local workspace="${2:-${NEEDLE_WORKSPACE:-$(pwd)}}"

    local bead_json
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        bead_json=$(cd "$workspace" && br show "$bead_id" --json 2>/dev/null)
    else
        bead_json=$(br show "$bead_id" --json 2>/dev/null)
    fi

    if [[ -z "$bead_json" ]]; then
        echo "0"
        return 0
    fi

    # Handle array or object response
    local labels_json
    if echo "$bead_json" | jq -e 'type == "array"' &>/dev/null; then
        labels_json=$(echo "$bead_json" | jq -r '.[0].labels // [] | .[]' 2>/dev/null)
    else
        labels_json=$(echo "$bead_json" | jq -r '.labels // [] | .[]' 2>/dev/null)
    fi

    # Find 'failure-count:N' label and extract N
    local count
    count=$(echo "$labels_json" | grep '^failure-count:' | sed 's/failure-count://' | head -1)
    if [[ -n "$count" ]] && [[ "$count" =~ ^[0-9]+$ ]]; then
        echo "$count"
    else
        echo "0"
    fi
}

# Increment the per-bead failure count label.
# Reads current count, increments, updates the bead label, and returns new count.
#
# Usage: _needle_increment_bead_failure_count <bead_id> [workspace]
# Outputs: New integer failure count
_needle_increment_bead_failure_count() {
    local bead_id="$1"
    local workspace="${2:-${NEEDLE_WORKSPACE:-$(pwd)}}"

    local current_count
    current_count=$(_needle_get_bead_failure_count "$bead_id" "$workspace")
    local new_count=$((current_count + 1))

    # Build update args: add new label, remove old one if present
    local update_args=("$bead_id" "--add-label" "failure-count:$new_count")
    if [[ "$current_count" -gt 0 ]]; then
        update_args+=("--remove-label" "failure-count:$current_count")
    fi

    if [[ -n "$workspace" && -d "$workspace" ]]; then
        (cd "$workspace" && br update "${update_args[@]}" &>/dev/null) || true
    else
        br update "${update_args[@]}" &>/dev/null || true
    fi

    echo "$new_count"
}

# Quarantine a bead from the pluck strand (permanently close, not retryable).
#
# Usage: _needle_quarantine_bead_pluck <bead_id> [reason] [workspace]
_needle_quarantine_bead_pluck() {
    local bead_id="$1"
    local reason="${2:-quarantined}"
    local workspace="${3:-${NEEDLE_WORKSPACE:-$(pwd)}}"

    _needle_debug "Quarantining bead: $bead_id (reason: $reason)"

    local update_result=1
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        (cd "$workspace" && br update "$bead_id" --status closed --add-label "quarantined" 2>/dev/null) && update_result=0
    else
        br update "$bead_id" --status closed --add-label "quarantined" 2>/dev/null && update_result=0
    fi

    if [[ $update_result -eq 0 ]]; then
        _needle_telemetry_emit "bead.quarantined" "error" \
            "bead_id=$bead_id" \
            "reason=$reason" \
            "session=${NEEDLE_SESSION:-unknown}" \
            "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

        _needle_warn "Bead quarantined: $bead_id (reason: $reason)"

        if declare -f _needle_run_hook &>/dev/null; then
            _needle_run_hook "on_quarantine" "$bead_id"
        fi

        return 0
    else
        _needle_error "Failed to quarantine bead: $bead_id"
        return 1
    fi
}

# ============================================================================
# Bead Processing Functions
# ============================================================================

# Process a claimed bead: mitosis check, dispatch, completion
#
# Usage: _needle_pluck_process_bead <bead_id> <workspace> <agent>
# Returns: 0 on success, 1 on failure
_needle_pluck_process_bead() {
    local bead_id="$1"
    local workspace="$2"
    local agent="$3"

    _needle_debug "Processing bead: $bead_id in workspace: $workspace"

    # Get bead details for title (needed for dispatch)
    # NOTE: br show must run in workspace context to find bead
    local bead_json bead_title
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        bead_json=$(cd "$workspace" && br show "$bead_id" --json 2>/dev/null)
    else
        bead_json=$(br show "$bead_id" --json 2>/dev/null)
    fi

    if [[ -z "$bead_json" ]] || [[ "$bead_json" == "null" ]]; then
        _needle_error "Could not retrieve bead details: $bead_id"
        return 1
    fi

    # Handle array or single object response
    local bead_object
    if echo "$bead_json" | jq -e 'type == "array"' &>/dev/null; then
        bead_object=$(echo "$bead_json" | jq -c '.[0]')
    else
        bead_object="$bead_json"
    fi

    bead_title=$(echo "$bead_object" | jq -r '.title // "Untitled Task"')
    local bead_type
    bead_type=$(echo "$bead_object" | jq -r '.issue_type // ""' 2>/dev/null || echo "")

    # Emit bead claimed event
    _needle_event_bead_claimed "$bead_id" \
        "workspace=$workspace" \
        "agent=$agent" \
        "title=$bead_title"

    # Step 1: Check mitosis
    _needle_debug "Checking mitosis for bead: $bead_id"
    _needle_event_bead_mitosis_check "$bead_id"

    if _needle_check_mitosis "$bead_id" "$workspace" "$agent"; then
        # Mitosis performed - bead was split into children
        # The mitosis module handles:
        # - Creating child beads
        # - Setting parent blocked by children
        # - Releasing parent claim
        # - Emitting mitosis events

        _needle_info "Mitosis performed on $bead_id, children created"
        _needle_event_bead_released "$bead_id" "reason=mitosis"

        # Mitosis is considered "work done" - return success
        # The children will be picked up in subsequent cycles
        return 0
    fi

    # Step 2: No mitosis - proceed with execution
    _needle_debug "No mitosis needed, proceeding with execution: $bead_id"

    # Step 3: Build prompt
    _needle_debug "Building prompt for bead: $bead_id"
    local prompt
    prompt=$(_needle_build_prompt "$bead_id" "$workspace")

    if [[ -z "$prompt" ]]; then
        _needle_error "Failed to build prompt for bead: $bead_id"
        _needle_mark_bead_failed "$bead_id" "prompt_build_failed" "" "$workspace" "$agent"
        return 1
    fi

    _needle_event_bead_prompt_built "$bead_id" \
        "workspace=$workspace" \
        "prompt_length=${#prompt}"

    # Step 4: Dispatch to agent
    _needle_debug "Dispatching to agent: $agent"
    _needle_event_bead_agent_started "$bead_id" \
        "agent=$agent" \
        "workspace=$workspace"

    local dispatch_result
    dispatch_result=$(_needle_dispatch_agent "$agent" "$workspace" "$prompt" "$bead_id" "$bead_title")

    local dispatch_exit=$?
    local exit_code duration output_file

    if [[ $dispatch_exit -ne 0 ]] || [[ -z "$dispatch_result" ]]; then
        _needle_error "Agent dispatch failed: $agent"
        _needle_event_error_dispatch_failed "$bead_id" \
            "agent=$agent" \
            "error=dispatch_failed"
        _needle_mark_bead_failed "$bead_id" "agent_dispatch_failed" "" "$workspace" "$agent"
        return 1
    fi

    # Parse dispatch result (last line only — prior lines are agent stdout via tee)
    local last_line
    last_line=$(tail -n 1 <<< "$dispatch_result")
    IFS='|' read -r exit_code duration output_file <<< "$last_line"

    _needle_debug "Agent completed: exit_code=$exit_code, duration=${duration}ms"

    # Step 4a: Extract tokens and record effort (updates daily_spend.json for needle analyze cost)
    local input_tokens=0
    local output_tokens=0
    local cost="0.00"

    if [[ -n "$output_file" ]] && [[ -f "$output_file" ]]; then
        local token_result
        token_result=$(_needle_extract_tokens "$agent" "$output_file")

        if [[ -n "$token_result" ]]; then
            input_tokens=$(echo "$token_result" | cut -d'|' -f1)
            output_tokens=$(echo "$token_result" | cut -d'|' -f2)
            [[ ! "$input_tokens" =~ ^[0-9]+$ ]] && input_tokens=0
            [[ ! "$output_tokens" =~ ^[0-9]+$ ]] && output_tokens=0
        fi

        cost=$(calculate_cost "$agent" "$input_tokens" "$output_tokens")
    fi

    if [[ "$input_tokens" -gt 0 ]] || [[ "$output_tokens" -gt 0 ]]; then
        record_effort "$bead_id" "$cost" "$agent" "$input_tokens" "$output_tokens" "pluck" "$bead_type"
        _needle_debug "Recorded effort: bead=$bead_id, cost=\$$cost, agent=$agent"
    fi

    # Step 5: Check exit code and mark bead appropriately
    if [[ "$exit_code" -eq 0 ]]; then
        # Step 5a: Run verification if verification_cmd is defined in the bead
        local verify_result verify_exit
        verify_result=$(_needle_verify_bead "$bead_id" "$workspace")
        verify_exit=$?

        if [[ $verify_exit -eq 0 ]] || [[ $verify_exit -eq 2 ]]; then
            # Passed verification (or no verification_cmd) — close the bead
            _needle_mark_bead_completed "$bead_id" "$output_file" "$duration" "$workspace"
            return 0
        fi

        # Verification failed — attempt one self-correction re-dispatch
        _needle_warn "Verification failed for $bead_id — attempting self-correction"
        _needle_telemetry_emit "bead.verify_self_correct" "warn" \
            "bead_id=$bead_id" \
            "workspace=$workspace"

        local failure_context
        failure_context=$(_needle_format_verification_failure_context "$verify_result")

        local correction_prompt
        correction_prompt="${prompt}

${failure_context}"

        local correction_result correction_dispatch_exit
        correction_result=$(_needle_dispatch_agent "$agent" "$workspace" "$correction_prompt" "$bead_id" "$bead_title")
        correction_dispatch_exit=$?

        if [[ $correction_dispatch_exit -eq 0 ]] && [[ -n "$correction_result" ]]; then
            local correction_last_line
            correction_last_line=$(tail -n 1 <<< "$correction_result")
            local corr_exit_code corr_duration corr_file
            IFS='|' read -r corr_exit_code corr_duration corr_file <<< "$correction_last_line"

            if [[ "$corr_exit_code" -eq 0 ]]; then
                # Record effort for correction pass
                if [[ -n "$corr_file" ]] && [[ -f "$corr_file" ]]; then
                    local corr_token_result corr_input_tokens=0 corr_output_tokens=0 corr_cost="0.00"
                    corr_token_result=$(_needle_extract_tokens "$agent" "$corr_file")
                    if [[ -n "$corr_token_result" ]]; then
                        corr_input_tokens=$(echo "$corr_token_result" | cut -d'|' -f1)
                        corr_output_tokens=$(echo "$corr_token_result" | cut -d'|' -f2)
                        [[ ! "$corr_input_tokens" =~ ^[0-9]+$ ]] && corr_input_tokens=0
                        [[ ! "$corr_output_tokens" =~ ^[0-9]+$ ]] && corr_output_tokens=0
                    fi
                    if [[ "$corr_input_tokens" -gt 0 ]] || [[ "$corr_output_tokens" -gt 0 ]]; then
                        corr_cost=$(calculate_cost "$agent" "$corr_input_tokens" "$corr_output_tokens")
                        record_effort "$bead_id" "$corr_cost" "$agent" "$corr_input_tokens" "$corr_output_tokens" "pluck" "$bead_type"
                        _needle_debug "Recorded correction effort: bead=$bead_id, cost=\$$corr_cost"
                    fi
                fi

                # Self-correction agent exited cleanly — re-verify
                local reverify_result reverify_exit
                reverify_result=$(_needle_verify_bead "$bead_id" "$workspace")
                reverify_exit=$?

                if [[ $reverify_exit -eq 0 ]] || [[ $reverify_exit -eq 2 ]]; then
                    # Self-correction succeeded — close the bead
                    _needle_info "Self-correction verified for $bead_id"
                    _needle_telemetry_emit "bead.verify_self_correct_passed" "info" \
                        "bead_id=$bead_id"
                    _needle_mark_bead_completed "$bead_id" "$corr_file" "$corr_duration" "$workspace"
                    return 0
                fi
            fi
        fi

        # Self-correction failed — release bead to queue for retry/mitosis path
        _needle_warn "Self-correction failed for $bead_id — releasing to queue"
        _needle_telemetry_emit "bead.verify_self_correct_failed" "error" \
            "bead_id=$bead_id" \
            "workspace=$workspace"
        _needle_release_bead "$bead_id" --reason "verification_failed_after_correction"
        return 1
    else
        # Failure - mark bead as blocked/failed
        _needle_mark_bead_failed "$bead_id" "exit_code_$exit_code" "$output_file" "$workspace" "$agent"
        return 1
    fi
}

# Mark a bead as successfully completed
#
# Usage: _needle_mark_bead_completed <bead_id> [output_file] [duration_ms] [workspace]
_needle_mark_bead_completed() {
    local bead_id="$1"
    local output_file="${2:-}"
    local duration="${3:-0}"
    local workspace="${4:-${NEEDLE_WORKSPACE:-$(pwd)}}"

    _needle_debug "Marking bead as completed: $bead_id"

    # Update bead status to closed
    # NOTE: br update must run in workspace context
    local update_result=1
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        (cd "$workspace" && br update "$bead_id" --status closed 2>/dev/null) && update_result=0
    else
        br update "$bead_id" --status closed 2>/dev/null && update_result=0
    fi
    if [[ $update_result -eq 0 ]]; then
        _needle_success "Bead completed: $bead_id (duration: ${duration}ms)"

        _needle_event_bead_completed "$bead_id" \
            "duration_ms=$duration" \
            "output_file=$output_file"

        # Record effort if we have duration
        if [[ -n "$duration" ]] && [[ "$duration" -gt 0 ]]; then
            _needle_event_effort_recorded "$bead_id" \
                "duration_ms=$duration"
        fi

        # Annotate bead with cost attribution from session logs.
        # Joins effort.recorded events back to the bead record so cost is
        # visible per-bead in `br show <id>`.
        if declare -f _needle_annotate_bead_with_effort &>/dev/null; then
            _needle_annotate_bead_with_effort "$bead_id" "$workspace" || true
        fi

        return 0
    else
        _needle_warn "Failed to mark bead as closed: $bead_id"
        return 1
    fi
}

# Mark a bead as failed/blocked.
# Before marking blocked, increments the per-bead failure counter and attempts
# forced mitosis when the failure threshold is reached. If forced mitosis
# cannot decompose the bead, it is quarantined instead.
#
# Usage: _needle_mark_bead_failed <bead_id> [reason] [output_file] [workspace] [agent]
_needle_mark_bead_failed() {
    local bead_id="$1"
    local reason="${2:-unknown}"
    local output_file="${3:-}"
    local workspace="${4:-${NEEDLE_WORKSPACE:-$(pwd)}}"
    local agent="${5:-${NEEDLE_LOOP_CURRENT_AGENT:-}}"

    _needle_debug "Marking bead as failed: $bead_id (reason: $reason)"

    # Track per-bead failure count via label
    local bead_failure_count
    bead_failure_count=$(_needle_increment_bead_failure_count "$bead_id" "$workspace")
    _needle_debug "Per-bead failure count for $bead_id: $bead_failure_count"

    # Check if forced mitosis should be attempted before falling back to block
    if [[ -n "$agent" ]] && _needle_mitosis_force_enabled "$workspace"; then
        local force_threshold
        force_threshold=$(_needle_mitosis_force_threshold "$workspace")

        if [[ "$bead_failure_count" -ge "$force_threshold" ]]; then
            _needle_warn "Bead $bead_id has failed $bead_failure_count time(s) — attempting forced mitosis (threshold=$force_threshold)"
            _needle_telemetry_emit "bead.force_mitosis.attempt" "warn" \
                "bead_id=$bead_id" \
                "failure_count=$bead_failure_count" \
                "threshold=$force_threshold" \
                "session=${NEEDLE_SESSION:-unknown}"

            if _needle_check_mitosis "$bead_id" "$workspace" "$agent" "true" "$bead_failure_count"; then
                # Mitosis succeeded — parent is now blocked by children
                _needle_info "Forced mitosis succeeded for $bead_id — parent blocked by children"
                _needle_telemetry_emit "bead.force_mitosis.success" "info" \
                    "bead_id=$bead_id" \
                    "failure_count=$bead_failure_count" \
                    "session=${NEEDLE_SESSION:-unknown}"
                return 0
            else
                # Mitosis cannot decompose — quarantine as truly atomic but consistently failing
                _needle_warn "Forced mitosis failed for $bead_id — quarantining (atomic bead, $bead_failure_count failures)"
                _needle_telemetry_emit "bead.force_mitosis.quarantine" "error" \
                    "bead_id=$bead_id" \
                    "failure_count=$bead_failure_count" \
                    "session=${NEEDLE_SESSION:-unknown}"
                _needle_quarantine_bead_pluck "$bead_id" "force_mitosis_exhausted" "$workspace"
                return 0
            fi
        fi
    fi

    # Standard failure path: mark as blocked with failed label
    # NOTE: br update must run in workspace context
    local update_result=1
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        (cd "$workspace" && br update "$bead_id" --status blocked --label failed --label "error:$reason" 2>/dev/null) && update_result=0
    else
        br update "$bead_id" --status blocked --label failed --label "error:$reason" 2>/dev/null && update_result=0
    fi
    if [[ $update_result -eq 0 ]]; then
        _needle_warn "Bead failed: $bead_id (reason: $reason)"

        _needle_event_bead_failed "$bead_id" \
            "reason=$reason" \
            "output_file=$output_file"

        return 0
    else
        _needle_error "Failed to update bead status: $bead_id"
        return 1
    fi
}

# ============================================================================
# Main Strand Entry Point
# ============================================================================

# Main pluck strand function
# Searches for work in configured workspaces and processes it
#
# Usage: _needle_strand_pluck <workspace> <agent>
# Arguments:
#   workspace - The primary workspace path
#   agent     - The agent identifier (e.g., "claude-anthropic-sonnet")
#
# Return values:
#   0 - Work was found and processed
#   1 - No work found (fallthrough to next strand)
_needle_strand_pluck() {
    local workspace="$1"
    local agent="$2"

    # DIAGNOSTIC: Log pluck strand invocation with full context
    _needle_diag_strand "pluck" "Pluck strand started" \
        "workspace=$workspace" \
        "agent=$agent" \
        "session=${NEEDLE_SESSION:-unknown}" \
        "needle_src=${NEEDLE_SRC:-unknown}" \
        "br_available=$(command -v br &>/dev/null && echo 'yes' || echo 'no')"

    _needle_debug "DIAG: pluck strand invoked - workspace=$workspace, agent=$agent, NEEDLE_SESSION=${NEEDLE_SESSION:-unknown}"
    _needle_debug "pluck strand: checking for primary work"

    # Validate inputs
    if [[ -z "$workspace" ]]; then
        _needle_error "pluck strand: workspace is required"
        _needle_diag_strand "pluck" "Pluck strand failed - missing workspace"
        return 1
    fi

    if [[ -z "$agent" ]]; then
        _needle_error "pluck strand: agent is required"
        _needle_diag_strand "pluck" "Pluck strand failed - missing agent"
        return 1
    fi

    # Get configured workspaces (with fallback to provided workspace)
    local workspaces
    workspaces=$(_needle_pluck_get_workspaces "$workspace")

    if [[ -z "$workspaces" ]]; then
        _needle_debug "pluck strand: no valid workspaces found"
        _needle_diag_strand "pluck" "No valid workspaces found" \
            "requested_workspace=$workspace"
        return 1
    fi

    # Count workspaces
    local ws_count
    ws_count=$(echo "$workspaces" | wc -l)

    _needle_diag_strand "pluck" "Found configured workspaces" \
        "workspace_count=$ws_count" \
        "workspaces=${workspaces//$'\n'/,}"

    # Track whether we did any work across all workspaces
    local work_done=false
    local ws_processed=0
    local ws_with_no_beads=0

    # Iterate through workspaces looking for work
    while IFS= read -r ws; do
        [[ -z "$ws" ]] && continue

        ((ws_processed++))

        _needle_verbose "pluck strand: checking workspace: $ws"
        _needle_diag_strand "pluck" "Checking workspace for beads" \
            "workspace=$ws" \
            "workspace_num=$ws_processed"

        # Attempt to claim a bead from this workspace
        local bead_id
        bead_id=$(_needle_claim_bead --workspace "$ws" --actor "$NEEDLE_SESSION")

        # DIAGNOSTIC: Log claim result
        _needle_debug "DIAG: claim_bead returned: ${bead_id:-<empty>}"
        _needle_diag_strand "pluck" "Claim attempt result" \
            "workspace=$ws" \
            "bead_id=${bead_id:-<none>}"

        if [[ -z "$bead_id" ]]; then
            _needle_verbose "pluck strand: no claimable beads in $ws"
            ((ws_with_no_beads++))
            continue
        fi

        _needle_info "Claimed bead: $bead_id from $ws"

        # Process the claimed bead
        if _needle_pluck_process_bead "$bead_id" "$ws" "$agent"; then
            work_done=true
            _needle_diag_strand "pluck" "Bead processed successfully" \
                "bead_id=$bead_id" \
                "workspace=$ws"
            # Return immediately on success - one bead at a time
            return 0
        else
            # Processing failed, but we did find work
            # Continue to next workspace or return with failure
            _needle_warn "Bead processing failed: $bead_id"
            _needle_diag_strand "pluck" "Bead processing failed" \
                "bead_id=$bead_id" \
                "workspace=$ws"
            work_done=true
            # Don't return here - the failure was recorded
            # Let fallthrough happen naturally
        fi

    done <<< "$workspaces"

    if $work_done; then
        # We found and attempted work (even if it failed)
        _needle_diag_strand "pluck" "Work attempted (may have failed)" \
            "workspaces_processed=$ws_processed" \
            "workspaces_with_no_beads=$ws_with_no_beads"
        return 0
    fi

    # No work found in any workspace
    _needle_debug "pluck strand: no work found in configured workspaces"
    _needle_diag_strand "pluck" "No work found in any workspace" \
        "workspaces_processed=$ws_processed" \
        "workspaces_with_no_beads=$ws_with_no_beads"

    _needle_diag_no_work "1" \
        "strand=pluck" \
        "workspaces_checked=$ws_processed" \
        "workspaces_empty=$ws_with_no_beads"

    return 1
}

# ============================================================================
# Utility Functions
# ============================================================================

# Get statistics about the pluck strand
# Usage: _needle_pluck_stats
# Returns: JSON object with stats
_needle_pluck_stats() {
    local workspaces
    workspaces=$(_needle_pluck_get_workspaces)

    local ws_count=0
    if [[ -n "$workspaces" ]]; then
        ws_count=$(echo "$workspaces" | wc -l)
    fi

    _needle_json_object \
        "configured_workspaces=$ws_count" \
        "strand=pluck" \
        "priority=1"
}

# ============================================================================
# Direct Execution Support (for testing)
# ============================================================================

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-}" in
        run)
            if [[ $# -lt 3 ]]; then
                echo "Usage: $0 run <workspace> <agent>"
                exit 1
            fi
            _needle_strand_pluck "$2" "$3"
            exit $?
            ;;
        workspaces)
            _needle_pluck_get_workspaces "${2:-}"
            ;;
        stats)
            _needle_pluck_stats | jq .
            ;;
        process)
            if [[ $# -lt 4 ]]; then
                echo "Usage: $0 process <bead_id> <workspace> <agent>"
                exit 1
            fi
            _needle_pluck_process_bead "$2" "$3" "$4"
            exit $?
            ;;
        -h|--help)
            echo "Usage: $0 <command> [args]"
            echo ""
            echo "Commands:"
            echo "  run <workspace> <agent>        Run the pluck strand"
            echo "  workspaces [fallback]          List configured workspaces"
            echo "  stats                          Show strand statistics"
            echo "  process <bead> <ws> <agent>    Process a specific bead"
            echo ""
            echo "The pluck strand:"
            echo "  1. Claims beads atomically from configured workspaces"
            echo "  2. Checks for mitosis (splits complex beads)"
            echo "  3. Dispatches to agent for execution"
            echo "  4. Marks beads complete or failed"
            ;;
        *)
            echo "Unknown command: ${1:-}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
fi
