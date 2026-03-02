#!/usr/bin/env bash
# NEEDLE Worker Loop Module
# Main worker loop that processes beads continuously
#
# This module implements the main processing loop that:
# 1. Runs the strand engine to find work
# 2. Claims and executes beads
# 3. Handles results (complete/fail)
# 4. Repeats until stopped
#
# Configuration:
#   runner.polling_interval - Time between iterations (default: 2s)
#   runner.idle_timeout - Exit after idle for N seconds (default: 300s)
#   runner.max_consecutive_empty - Max empty iterations before backoff (default: 5)
#
# Shutdown signals:
#   - SIGTERM: Graceful shutdown
#   - SIGINT: Graceful shutdown
#   - SIGHUP: Graceful shutdown
#
# Exit codes:
#   0 - Success
#   1 - Error (general)
#   2 - Bead processing failed
#   130 - Interrupt received
#
# Return values from strand engine:
#   0 - Work was found and processed
#   1 - No work found (fallthrough to next strand)
#
# Return values from _needle_process_bead:
#   0 - Bead processed successfully
#   1 - Bead processing failed
#   2 - Bead skipped (hook abort)
#   3 - Bead released (from queue)
#
# Environment variables (set by runner before loop starts):
#   NEEDLE_SESSION    - Unique session identifier
#   NEEDLE_RUNNER     - Runner type (e.g., claude)
#   NEEDLE_PROVIDER   - AI provider (e.g., anthropic)
#   NEEDLE_MODEL      - Model identifier (e.g., sonnet)
#   NEEDLE_IDENTIFIER - Instance identifier (e.g., alpha)
#   NEEDLE_WORKSPACE  - Current workspace path
#   NEEDLE_AGENT      - Current agent name
#   NEEDLE_LOG_FILE   - Log file for telemetry
#   NEEDLE_STATE_DIR  - State directory path
#   NEEDLE_HOME       - NEEDLE home directory

# Get NEEDLE_SRC if not already set
if [[ -z "${NEEDLE_SRC:-}" ]]; then
    NEEDLE_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi

_NEEDLE_LOOP_LOADED=true
_NEEDLE_LOOP_INIT=false
_NEEDLE_LOOP_SHUTDOWN=false
_NEEDLE_LOOP_DRAINING=false
_NEEDLE_LOOP_INTERRUPT=false

# ============================================================================
# Configuration Defaults
# ============================================================================

NEEDLE_LOOP_DEFAULT_POLLING_INTERVAL="2s"
NEEDLE_LOOP_DEFAULT_IDLE_TIMEOUT="300s"
NEEDLE_LOOP_DEFAULT_MAX_EMPTY="5"
NEEDLE_LOOP_SHUTDOWN_GRACE_PERIOD="5"

# ============================================================================
# Signal Handlers
# ============================================================================

# Set up signal handlers for graceful termination
# Usage: _needle_loop_setup_signals
_needle_loop_setup_signals() {
    trap '_needle_loop_handle_shutdown TERM' TERM
    trap '_needle_loop_handle_shutdown INT' INT
    trap '_needle_loop_handle_shutdown HUP' HUP
    _needle_debug "Signal handlers installed for graceful shutdown"
}

# Handle shutdown signal
# Usage: _needle_loop_handle_shutdown <signal>
_needle_loop_handle_shutdown() {
    local signal="$1"

    _NEEDLE_LOOP_SHUTDOWN=true
    _NEEDLE_LOOP_DRAINING=true

    _needle_warn "Received $signal, initiating graceful shutdown..."

    # Emit draining event
    _needle_event_worker_draining

    # Emit telemetry event
    _needle_telemetry_emit "worker.shutdown_initiated" \
        "signal=$signal" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    # Start grace period countdown
    local grace_period="$NEEDLE_LOOP_SHUTDOWN_GRACE_PERIOD"
    _needle_info "Allowing $grace_period seconds for cleanup..."

    # Give processes time to finish
    sleep "$grace_period"

    # Clear the flag
    _NEEDLE_LOOP_SHUTDOWN=false
}

# ============================================================================
# Configuration Helper
# ============================================================================

# Get configuration value with fallback
# Usage: _needle_loop_get_config <key> <default>
_needle_loop_get_config() {
    local key="$1"
    local default="$2"

    # Try get_config from config.sh if available
    if declare -f get_config &>/dev/null; then
        get_config "$key" "$default"
    else
        echo "$default"
    fi
}

# ============================================================================
# Event Emission Stubs
# ============================================================================

# Emit worker started event (stub)
_needle_event_worker_started() {
    _needle_debug "Worker started: $*"
}

# Emit worker idle event (stub)
_needle_event_worker_idle() {
    _needle_debug "Worker idle: $*"
}

# Emit worker stopped event (stub)
_needle_event_worker_stopped() {
    _needle_debug "Worker stopped: $*"
}

# Emit worker draining event (stub)
_needle_event_worker_draining() {
    _needle_debug "Worker draining"
}

# Emit bead claimed event (stub)
_needle_event_bead_claimed() {
    _needle_debug "Bead claimed: $*"
}

# Emit bead prompt built event (stub)
_needle_event_bead_prompt_built() {
    _needle_debug "Bead prompt built: $*"
}

# Emit bead agent started event (stub)
_needle_event_bead_agent_started() {
    _needle_debug "Bead agent started: $*"
}

# Emit bead agent completed event (stub)
_needle_event_bead_agent_completed() {
    _needle_debug "Bead agent completed: $*"
}

# Emit bead completed event (stub)
_needle_event_bead_completed() {
    _needle_debug "Bead completed: $*"
}

# Emit bead failed event (stub)
_needle_event_bead_failed() {
    _needle_debug "Bead failed: $*"
}

# Emit bead released event (stub)
_needle_event_bead_released() {
    _needle_debug "Bead released: $*"
}

# Emit error events (stubs)
_needle_event_error_claim_failed() {
    _needle_warn "Error: claim failed: $*"
}

_needle_event_error_complete_failed() {
    _needle_warn "Error: complete failed: $*"
}

_needle_event_error_release_failed() {
    _needle_warn "Error: release failed: $*"
}

# ============================================================================
# Telemetry Emission Stubs
# ============================================================================

# Emit telemetry event (stub)
_needle_telemetry_emit() {
    _needle_debug "Telemetry: $*"
}

# Initialize telemetry (stub)
_needle_telemetry_init() {
    _needle_debug "Telemetry initialized"
}

# ============================================================================
# Heartbeat Stubs
# ============================================================================

# Heartbeat keepalive (stub)
_needle_heartbeat_keepalive() {
    _needle_debug "Heartbeat keepalive"
}

# Heartbeat start bead (stub)
_needle_heartbeat_start_bead() {
    _needle_debug "Heartbeat start bead: $*"
}

# Heartbeat end bead (stub)
_needle_heartbeat_end_bead() {
    _needle_debug "Heartbeat end bead"
}

# Heartbeat draining (stub)
_needle_heartbeat_draining() {
    _needle_debug "Heartbeat draining"
}

# Heartbeat cleanup (stub)
_needle_heartbeat_cleanup() {
    _needle_debug "Heartbeat cleanup"
}

# Heartbeat init (stub)
_needle_heartbeat_init() {
    _needle_debug "Heartbeat initialized"
}

# ============================================================================
# Strand Engine Stub
# ============================================================================

# Run strand engine to find work (stub)
# Returns: 0 if work found, 1 if no work
_needle_strand_engine() {
    local workspace="$1"
    local agent="$2"

    _needle_debug "Strand engine checking for work in $workspace with agent $agent"
    # TODO: Implement actual strand engine call
    # For now, return 1 (no work found)
    return 1
}

# ============================================================================
# Bead Processing Stubs
# ============================================================================

# Build prompt for agent (stub)
# Returns: prompt string
_needle_build_prompt() {
    local bead_id="$1"
    local workspace="$2"

    _needle_debug "Building prompt for bead $bead_id in $workspace"
    # TODO: Implement actual prompt building
    echo "Process bead $bead_id in workspace $workspace"
}

# Dispatch agent (stub)
# Returns: exit_code|duration_ms|output_file
_needle_dispatch_agent() {
    local agent="$1"
    local workspace="$2"
    local prompt="$3"
    local bead_id="$4"
    local bead_title="$5"

    _needle_debug "Dispatching agent $agent for bead $bead_id"
    # TODO: Implement actual agent dispatch
    # Return: exit_code|duration_ms|output_file
    echo "0|100|/tmp/needle-output-${bead_id}.log"
}

# Run hook (stub)
# Returns: 0 on success
_needle_run_hook() {
    local hook_name="$1"
    local bead_id="$2"

    _needle_debug "Running hook $hook_name for bead $bead_id"
    # TODO: Implement actual hook runner call
    return 0
}

# Release bead (stub)
_needle_release_bead() {
    local bead_id="$1"
    local reason="$2"

    _needle_debug "Releasing bead $bead_id: $reason"
    # TODO: Implement actual release
}

# Unregister worker (stub)
_needle_unregister_worker() {
    local session="$1"
    _needle_debug "Unregistering worker: $session"
}

# Register worker (stub)
_needle_register_worker() {
    _needle_debug "Registering worker: $*"
}

# ============================================================================
# Main Worker Loop
# ============================================================================

# Initialize the worker loop
# Usage: _needle_worker_loop_init
_needle_worker_loop_init() {
    _needle_debug "Initializing worker loop..."

    # Set up signal handlers
    _needle_loop_setup_signals

    # Initialize heartbeat system
    _needle_heartbeat_init

    # Register this worker in the state
    _needle_register_worker \
        "$NEEDLE_SESSION" \
        "$NEEDLE_RUNNER" \
        "$NEEDLE_PROVIDER" \
        "$NEEDLE_MODEL" \
        "$NEEDLE_IDENTIFIER" \
        "$$" \
        "$NEEDLE_WORKSPACE"

    _NEEDLE_LOOP_INIT=true
    _needle_debug "Worker loop initialized"
}

# Main worker loop - processes beads continuously
# Usage: _needle_worker_loop <workspace> <agent>
# Arguments:
#   workspace - The workspace path to process
#   agent     - The agent to use for execution
# Return values:
#   0 - Success (worker ran until shutdown)
#   1 - Error (initialization failed)
#   130 - Interrupt received (shutdown)
_needle_worker_loop() {
    local workspace="$1"
    local agent="$2"

    # Validate inputs
    if [[ -z "$workspace" ]]; then
        _needle_error "Workspace is required for worker loop"
        return 1
    fi
    if [[ -z "$agent" ]]; then
        _needle_error "Agent is required for worker loop"
        return 1
    fi

    # Initialize the worker loop
    _needle_worker_loop_init

    # Emit worker started event
    _needle_event_worker_started \
        "workspace=$workspace" \
        "agent=$agent"

    # Also emit telemetry event
    _needle_telemetry_emit "worker.started" \
        "workspace=$workspace" \
        "agent=$agent" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    local consecutive_empty=0
    local idle_start=""

    # Get configuration values
    local polling_interval
    polling_interval=$(_needle_loop_get_config "runner.polling_interval" "$NEEDLE_LOOP_DEFAULT_POLLING_INTERVAL")

    local idle_timeout
    idle_timeout=$(_needle_loop_get_config "runner.idle_timeout" "$NEEDLE_LOOP_DEFAULT_IDLE_TIMEOUT")

    # Remove 's' suffix
    polling_interval="${polling_interval%s}"
    idle_timeout="${idle_timeout%s}"

    local max_consecutive_empty
    max_consecutive_empty=$(_needle_loop_get_config "runner.max_consecutive_empty" "$NEEDLE_LOOP_DEFAULT_MAX_EMPTY")

    _needle_debug "Configuration: polling_interval=${polling_interval}s, idle_timeout=${idle_timeout}s"
    _needle_debug "Starting worker loop for workspace: $workspace, agent: $agent"

    # Main processing loop
    while true; do
        # Check for shutdown signal
        if [[ "$_NEEDLE_LOOP_SHUTDOWN" == "true" ]]; then
            _needle_debug "Shutdown signal detected, entering draining state"
            _NEEDLE_LOOP_DRAINING=true

            # Emit idle heartbeat and update status
            _needle_heartbeat_draining

            # Break out of loop after grace period
            local grace_period="$NEEDLE_LOOP_SHUTDOWN_GRACE_PERIOD"
            _needle_info "Shutdown requested, allowing ${grace_period}s for cleanup..."
            sleep "$grace_period"
            _needle_event_worker_stopped "reason=shutdown"
            break
        fi

        # Check for shutdown file
        local shutdown_file="$NEEDLE_STATE_DIR/shutdown_$NEEDLE_IDENTIFIER"
        if [[ -f "$shutdown_file" ]]; then
            _needle_debug "Shutdown file detected: stopping worker"
            break
        fi

        # Emit heartbeat keepalive
        _needle_heartbeat_keepalive

        # Run strand engine to find work
        local strand_result
        if _needle_strand_engine "$workspace" "$agent"; then
            strand_result=$?
        else
            strand_result=$?
        fi

        if [[ $strand_result -eq 0 ]]; then
            # Work found and processed
            consecutive_empty=0
            idle_start=""
            _needle_debug "Strand engine found work"
        elif [[ "$strand_result" -eq 1 ]]; then
            # No work found
            ((consecutive_empty++))

            # Track idle time
            if [[ -z "$idle_start" ]]; then
                idle_start=$(date +%s)
            fi

            local idle_seconds=$(($(date +%s) - idle_start))

            _needle_event_worker_idle \
                "consecutive_empty=$consecutive_empty" \
                "idle_seconds=$idle_seconds" \
                "workspace=$workspace" \
                "agent=$agent"

            _needle_debug "Worker idle: consecutive_empty=$consecutive_empty, idle_seconds=${idle_seconds}s"

            # Check idle timeout
            if ((idle_seconds >= idle_timeout)); then
                _needle_debug "Idle timeout reached (${idle_seconds}s >= ${idle_timeout}s)"
                _needle_event_worker_stopped "reason=idle_timeout"
                _needle_telemetry_emit "worker.stopped" \
                    "reason=idle_timeout" \
                    "session=$NEEDLE_SESSION" \
                    "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
                break
            fi
        fi

        # Wait before next iteration (unless draining)
        if [[ "$_NEEDLE_LOOP_DRAINING" != "true" ]]; then
            local sleep_seconds="${polling_interval%s}"
            _needle_debug "Sleeping for ${sleep_seconds}s..."
            sleep "$sleep_seconds"
        fi
    done

    # Emit final telemetry event
    _needle_telemetry_emit "worker.stopped" \
        "reason=shutdown" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    # Clean up heartbeat
    _needle_heartbeat_cleanup

    # Unregister worker
    _needle_unregister_worker "$NEEDLE_SESSION"

    # Log final event
    _needle_event_worker_stopped "reason=shutdown"
    _needle_success "Worker loop completed"
    _NEEDLE_LOOP_INIT=false
    return 0
}

# ============================================================================
# Bead Processing Functions
# ============================================================================

# Process a single bead - runs hooks, builds prompt, executes agent, handles result
# Usage: _needle_process_bead <bead_id> <workspace> <agent>
# Arguments:
#   bead_id   - The bead ID to process
#   workspace - The workspace path
#   agent     - The agent to use for execution
# Return values:
#   0 - Bead processed successfully
#   1 - Bead processing failed
#   2 - Bead skipped (hook abort)
#   3 - Bead released (from queue)
_needle_process_bead() {
    local bead_id="$1"
    local workspace="$2"
    local agent="$3"

    _needle_debug "Processing bead: $bead_id in workspace: $workspace"

    # Emit bead claimed event
    _needle_event_bead_claimed "$bead_id" "workspace=$workspace"

    # Also emit telemetry event
    _needle_telemetry_emit "bead.claimed" \
        "bead_id=$bead_id" \
        "workspace=$workspace" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    # Build prompt for agent
    local prompt
    prompt=$(_needle_build_prompt "$bead_id" "$workspace")

    if [[ $? -ne 0 ]]; then
        _needle_error "Failed to build prompt for bead: $bead_id"
        _needle_event_error_claim_failed "$bead_id" "reason=prompt_build_failed"
        return 1
    fi

    # Get bead title
    local bead_json
    bead_json=$(br show "$bead_id" --json 2>/dev/null)
    local bead_title
    bead_title=$(echo "$bead_json" | jq -r '.title // "Untitled"' 2>/dev/null)

    # Emit prompt built event
    local prompt_length=${#prompt}
    _needle_event_bead_prompt_built "$bead_id" "prompt_length=$prompt_length"
    _needle_telemetry_emit "bead.prompt_built" \
        "bead_id=$bead_id" \
        "prompt_length=$prompt_length" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    _needle_debug "Prompt built for bead $bead_id (${prompt_length} chars)"

    # Run pre-execute hook
    if ! _needle_run_hook "pre_execute" "$bead_id"; then
        _needle_warn "Pre-execute hook aborted for bead $bead_id"
        _needle_release_bead "$bead_id" "hook_failed"
        return 1
    fi

    # Update heartbeat to show bead is being executed
    _needle_heartbeat_start_bead "$bead_id"

    # Emit agent started event
    _needle_event_bead_agent_started "$bead_id" "agent=$agent"
    _needle_telemetry_emit "bead.agent_started" \
        "bead_id=$bead_id" \
        "agent=$agent" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    # Dispatch to agent
    local result
    result=$(_needle_dispatch_agent "$agent" "$workspace" "$prompt" "$bead_id" "$bead_title")
    local dispatch_exit=$?
    local dispatch_duration=0
    local dispatch_output=""
    IFS='|' read -r dispatch_exit dispatch_duration dispatch_output <<< "$result"

    # Update heartbeat to show completion
    _needle_heartbeat_end_bead

    # Emit agent completed event
    _needle_event_bead_agent_completed "$bead_id" \
        "exit_code=$dispatch_exit" \
        "duration_ms=$dispatch_duration"
    _needle_telemetry_emit "bead.agent_completed" \
        "bead_id=$bead_id" \
        "exit_code=$dispatch_exit" \
        "duration_ms=$dispatch_duration" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    # Run post-execute hook
    # Set environment variables for the hook
    export NEEDLE_EXIT_CODE="$dispatch_exit"
    export NEEDLE_DURATION_MS="$dispatch_duration"
    export NEEDLE_OUTPUT_FILE="$dispatch_output"

    if ! _needle_run_hook "post_execute" "$bead_id"; then
        _needle_warn "Post-execute hook failed for bead $bead_id"
    fi

    # Handle result based on exit code
    if [[ "$dispatch_exit" -eq 0 ]]; then
        _needle_complete_bead "$bead_id" "$dispatch_output"
    else
        _needle_fail_bead "$bead_id" "$dispatch_exit" "$dispatch_output"
    fi

    # Cleanup output file
    if [[ -n "$dispatch_output" ]] && [[ -f "$dispatch_output" ]]; then
        rm -f "$dispatch_output"
    fi

    _needle_debug "Completed processing bead: $bead_id"
    return 0
}

# ============================================================================
# Bead Completion Functions
# ============================================================================

# Complete a bead successfully
# Usage: _needle_complete_bead <bead_id> [output_file]
# Return values:
#   0 - Bead completed successfully
#   1 - Completion failed (hook abort or br error)
_needle_complete_bead() {
    local bead_id="$1"
    local output_file="${2:-}"

    _needle_debug "Completing bead: $bead_id"

    # Run pre-complete hook (quality gates)
    if ! _needle_run_hook "pre_complete" "$bead_id"; then
        _needle_warn "Pre-complete hook aborted for bead $bead_id"
        _needle_fail_bead "$bead_id" "hook_failed"
        return 1
    fi

    # Mark as completed using br CLI
    if ! br update "$bead_id" --status closed 2>/dev/null; then
        _needle_error "Failed to close bead: $bead_id"
        _needle_event_error_complete_failed "$bead_id" "reason=br_command_failed"
        return 1
    fi

    # Emit completed event
    _needle_event_bead_completed "$bead_id"
    _needle_telemetry_emit "bead.completed" \
        "bead_id=$bead_id" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    _needle_success "Bead completed: $bead_id"

    # Run post-complete hook
    _needle_run_hook "post_complete" "$bead_id"

    return 0
}

# ============================================================================
# Bead Failure Functions
# ============================================================================

# Fail a bead and release it back to queue
# Usage: _needle_fail_bead <bead_id> <reason> [output_file]
# Return values:
#   0 - Bead failed and released successfully
#   1 - Release failed
_needle_fail_bead() {
    local bead_id="$1"
    local reason="$2"
    local output_file="${3:-}"

    _needle_debug "Failing bead: $bead_id (reason: $reason)"

    # Release back to queue using br CLI
    if ! br update "$bead_id" --release --reason "$reason" 2>/dev/null; then
        _needle_error "Failed to release bead: $bead_id"
        _needle_event_error_release_failed "$bead_id" "reason=release_command_failed"
        # Still try to run on_failure hook
        _needle_run_hook "on_failure" "$bead_id"
        return 1
    fi

    # Emit failed event
    _needle_event_bead_failed "$bead_id" "reason=$reason"
    _needle_telemetry_emit "bead.failed" \
        "bead_id=$bead_id" \
        "reason=$reason" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    _needle_warn "Bead failed: $bead_id (reason: $reason)"

    # Run on_failure hook
    _needle_run_hook "on_failure" "$bead_id"

    return 0
}

# ============================================================================
# Bead Release Function
# ============================================================================

# Release a bead back to the queue without failure
# Usage: _needle_release_bead <bead_id> <reason>
# Return values:
#   0 - Bead released successfully
#   1 - Release failed
_needle_release_bead() {
    local bead_id="$1"
    local reason="$2"

    _needle_debug "Releasing bead: $bead_id (reason: $reason)"

    # Release using br CLI
    if ! br update "$bead_id" --release --reason "$reason" 2>/dev/null; then
        _needle_error "Failed to release bead: $bead_id"
        _needle_event_error_release_failed "$bead_id" "reason=$reason"
        return 1
    fi

    # Emit released event
    _needle_event_bead_released "$bead_id" "reason=$reason"
    _needle_telemetry_emit "bead.released" \
        "bead_id=$bead_id" \
        "reason=$reason" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    _needle_info "Bead released: $bead_id (reason: $reason)"

    return 0
}

# ============================================================================
# Direct Execution Support (for testing)
# ============================================================================

# Allow running this module directly for testing
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-}" in
        run)
            if [[ -z "${2:-}" ]] || [[ -z "${3:-}" ]]; then
                echo "Usage: $0 run <workspace> <agent>"
                exit 1
            fi
            # Set up environment for testing
            NEEDLE_SESSION="test-session"
            NEEDLE_RUNNER="test"
            NEEDLE_PROVIDER="test"
            NEEDLE_MODEL="test"
            NEEDLE_IDENTIFIER="test"
            export NEEDLE_SESSION NEEDLE_RUNNER NEEDLE_PROVIDER NEEDLE_MODEL NEEDLE_IDENTIFIER
            # Initialize telemetry
            _needle_telemetry_init
            # Run the worker loop
            _needle_worker_loop "$2" "$3"
            ;;
        process)
            if [[ -z "${2:-}" ]] || [[ -z "${3:-}" ]] || [[ -z "${4:-}" ]]; then
                echo "Usage: $0 process <bead_id> <workspace> <agent>"
                exit 1
            fi
            # Set up environment for testing
            NEEDLE_SESSION="test-session"
            NEEDLE_RUNNER="test"
            NEEDLE_PROVIDER="test"
            NEEDLE_MODEL="sleep"
            NEEDLE_IDENTIFIER="test"
            export NEEDLE_SESSION NEEDLE_RUNNER NEEDLE_PROVIDER NEEDLE_MODEL NEEDLE_IDENTIFIER
            _needle_process_bead "$2" "$3" "$4"
            ;;
        complete)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 complete <bead_id>"
                exit 1
            fi
            _needle_complete_bead "$2"
            ;;
        fail)
            if [[ -z "${2:-}" ]] || [[ -z "${3:-}" ]]; then
                echo "Usage: $0 fail <bead_id> <reason>"
                exit 1
            fi
            _needle_fail_bead "$2" "$3"
            ;;
        release)
            if [[ -z "${2:-}" ]] || [[ -z "${3:-}" ]]; then
                echo "Usage: $0 release <bead_id> <reason>"
                exit 1
            fi
            _needle_release_bead "$2" "$3"
            ;;
        -h|--help)
            cat <<'EOF'
Needle Worker Loop - Direct Execution Support

Usage: $0 <command> [args]

Commands:
  run <workspace> <agent>
      Run the worker loop with the specified workspace and agent

  process <bead_id> <workspace> <agent>
      Process a single bead

  complete <bead_id>
      Mark a bead as completed

  fail <bead_id> <reason>
      Mark a bead as failed and release it

  release <bead_id> <reason>
      Release a bead back to queue

Environment Variables (required):
  NEEDLE_SESSION    - Unique session identifier
  NEEDLE_RUNNER     - Runner type
  NEEDLE_PROVIDER   - AI provider
  NEEDLE_MODEL      - Model identifier
  NEEDLE_IDENTIFIER - Instance identifier
  NEEDLE_HOME       - NEEDLE home directory
EOF
            ;;
        *)
            echo "Unknown command: ${1:-}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
fi
