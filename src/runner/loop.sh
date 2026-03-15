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

# ============================================================================
# PATH Setup (CRITICAL: Must be done before any br calls)
# ============================================================================
# Ensure ~/.local/bin is in PATH for br CLI access
# This fixes worker starvation caused by br not being found
if [[ -d "$HOME/.local/bin" ]]; then
    case ":$PATH:" in
        *":$HOME/.local/bin:"*) ;;
        *) export PATH="$HOME/.local/bin:$PATH" ;;
    esac
fi

# Get NEEDLE_SRC if not already set
if [[ -z "${NEEDLE_SRC:-}" ]]; then
    NEEDLE_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi

# ============================================================================
# Source Dependencies
# ============================================================================
# Source lib modules first (provides output, json, utils, config)
source "$NEEDLE_SRC/lib/output.sh"
source "$NEEDLE_SRC/lib/json.sh"
source "$NEEDLE_SRC/lib/utils.sh"
source "$NEEDLE_SRC/lib/config.sh"

# Source telemetry/events module for event emission
source "$NEEDLE_SRC/telemetry/events.sh"

# Source heartbeat module for worker liveness tracking
source "$NEEDLE_SRC/watchdog/heartbeat.sh"

# Source state registry for worker registration
source "$NEEDLE_SRC/runner/state.sh"

# Source bead modules for prompt building and claiming
source "$NEEDLE_SRC/bead/claim.sh"
source "$NEEDLE_SRC/bead/prompt.sh"

# Source agent modules for dispatching
source "$NEEDLE_SRC/agent/loader.sh"
source "$NEEDLE_SRC/agent/dispatch.sh"

# Source hook runner for pre/post execution hooks
source "$NEEDLE_SRC/hooks/runner.sh"

# Source telemetry modules for token extraction and effort tracking
source "$NEEDLE_SRC/telemetry/tokens.sh"
source "$NEEDLE_SRC/telemetry/effort.sh"

# Source quality modules for bug scanning
source "$NEEDLE_SRC/quality/bug_scanner.sh"

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

# Runtime config values (updated on hot-reload and workspace config changes)
NEEDLE_LOOP_CURRENT_POLLING_INTERVAL="2"
NEEDLE_LOOP_CURRENT_IDLE_TIMEOUT="300"
NEEDLE_LOOP_CURRENT_MAX_CONSECUTIVE_EMPTY="5"

# ============================================================================
# Backoff and Crash Recovery Configuration
# ============================================================================

# Backoff timing for repeated failures
NEEDLE_BACKOFF_BASE_SECONDS=30
NEEDLE_BACKOFF_MAX_SECONDS=120
NEEDLE_BACKOFF_MULTIPLIER=2

# Failure thresholds
NEEDLE_BACKOFF_THRESHOLD=3       # Start backoff after this many failures
NEEDLE_ALERT_THRESHOLD=5         # Alert human after this many failures
NEEDLE_MAX_FAILURES=7            # Exit worker after this many failures

# State variables (reset per-session)
NEEDLE_FAILURE_COUNT=0
NEEDLE_BACKOFF_SECONDS=0
NEEDLE_LAST_FAILURE_TIME=""

# ============================================================================
# Configuration Hot-Reload State
# ============================================================================

# Track modification times for config files
NEEDLE_GLOBAL_CONFIG_LOADED_AT=0
NEEDLE_WS_CONFIG_LOADED_AT=0

# Config check interval (checked every N loop iterations)
# This avoids checking file mtime on every iteration
NEEDLE_CONFIG_CHECK_INTERVAL=15
NEEDLE_CONFIG_CHECK_COUNTER=0

# ============================================================================
# Backoff and Crash Recovery Functions
# ============================================================================

# Reset backoff state (call on successful bead completion)
# Usage: _needle_reset_backoff
_needle_reset_backoff() {
    NEEDLE_FAILURE_COUNT=0
    NEEDLE_BACKOFF_SECONDS=0
    NEEDLE_LAST_FAILURE_TIME=""
    _needle_debug "Backoff state reset (failure count cleared)"
}

# Increment backoff after a failure
# Implements exponential backoff: 30s -> 60s -> 120s (max)
# Usage: _needle_increment_backoff
# Returns: Number of seconds to sleep (also stored in NEEDLE_BACKOFF_SECONDS)
_needle_increment_backoff() {
    ((NEEDLE_FAILURE_COUNT++)) || true
    NEEDLE_LAST_FAILURE_TIME=$(date -u +%Y-%m-%dT%H:%M:%SZ)

    _needle_debug "Failure count incremented to: $NEEDLE_FAILURE_COUNT"

    # Calculate exponential backoff
    if [[ $NEEDLE_FAILURE_COUNT -ge $NEEDLE_BACKOFF_THRESHOLD ]]; then
        # Calculate backoff: base * (multiplier ^ (failures - threshold))
        local exponent=$((NEEDLE_FAILURE_COUNT - NEEDLE_BACKOFF_THRESHOLD))
        NEEDLE_BACKOFF_SECONDS=$((NEEDLE_BACKOFF_BASE_SECONDS * (NEEDLE_BACKOFF_MULTIPLIER ** exponent)))

        # Cap at maximum
        if [[ $NEEDLE_BACKOFF_SECONDS -gt $NEEDLE_BACKOFF_MAX_SECONDS ]]; then
            NEEDLE_BACKOFF_SECONDS=$NEEDLE_BACKOFF_MAX_SECONDS
        fi

        _needle_warn "Backoff activated: ${NEEDLE_BACKOFF_SECONDS}s (failure #$NEEDLE_FAILURE_COUNT)"
    else
        NEEDLE_BACKOFF_SECONDS=0
    fi

    echo "$NEEDLE_BACKOFF_SECONDS"
}

# Check if worker should alert human for repeated failures
# Usage: _needle_should_alert_human
# Returns: 0 if should alert, 1 if not
_needle_should_alert_human() {
    [[ $NEEDLE_FAILURE_COUNT -ge $NEEDLE_ALERT_THRESHOLD ]]
}

# Check if worker should exit due to excessive failures
# Usage: _needle_should_exit_worker
# Returns: 0 if should exit, 1 if not
_needle_should_exit_worker() {
    [[ $NEEDLE_FAILURE_COUNT -ge $NEEDLE_MAX_FAILURES ]]
}

# Apply backoff delay if needed
# Usage: _needle_apply_backoff
_needle_apply_backoff() {
    if [[ $NEEDLE_BACKOFF_SECONDS -gt 0 ]]; then
        _needle_warn "Applying backoff: sleeping for ${NEEDLE_BACKOFF_SECONDS}s..."

        # Emit backoff event for telemetry
        _needle_telemetry_emit "worker.backoff" "warn" \
            "failure_count=$NEEDLE_FAILURE_COUNT" \
            "backoff_seconds=$NEEDLE_BACKOFF_SECONDS" \
            "session=$NEEDLE_SESSION"

        sleep "$NEEDLE_BACKOFF_SECONDS"
    fi
}

# ============================================================================
# Configuration Hot-Reload Functions
# ============================================================================

# Get file modification time as epoch seconds
# Returns 0 if file doesn't exist
# Usage: _needle_get_config_mtime <file_path>
_needle_get_config_mtime() {
    local file_path="$1"

    if [[ -f "$file_path" ]]; then
        stat -c %Y "$file_path" 2>/dev/null || echo 0
    else
        echo 0
    fi
}

# Initialize config tracking timestamps
# Called once at worker startup
# Usage: _needle_init_config_tracking
_needle_init_config_tracking() {
    local global_config="${NEEDLE_HOME:-$HOME/.needle}/config.yaml"

    NEEDLE_GLOBAL_CONFIG_LOADED_AT=$(_needle_get_config_mtime "$global_config")

    # Initialize workspace config mtime if workspace is set
    if [[ -n "${NEEDLE_WORKSPACE:-}" ]]; then
        local ws_config="$NEEDLE_WORKSPACE/.needle.yaml"
        NEEDLE_WS_CONFIG_LOADED_AT=$(_needle_get_config_mtime "$ws_config")
    else
        NEEDLE_WS_CONFIG_LOADED_AT=0
    fi

    _needle_debug "Config tracking initialized: global=$NEEDLE_GLOBAL_CONFIG_LOADED_AT, workspace=$NEEDLE_WS_CONFIG_LOADED_AT"
}

# Check if configuration files have changed and reload if needed
# Uses file mtime comparison - no inotify dependency
# Called periodically from the main loop
# Usage: _needle_check_config_reload
# Returns: Always 0 (compatible with set -e)
# Side effect: Reloads config if changes detected
_needle_check_config_reload() {
    local global_config="${NEEDLE_HOME:-$HOME/.needle}/config.yaml"
    local ws_config=""
    local reload_needed=false
    local reload_sources=()

    # Check if we should skip this check (only check every N iterations)
    # Note: Use || true to prevent set -e from triggering when counter is 0
    ((NEEDLE_CONFIG_CHECK_COUNTER++)) || true
    if [[ $NEEDLE_CONFIG_CHECK_COUNTER -lt $NEEDLE_CONFIG_CHECK_INTERVAL ]]; then
        return 1  # Not yet time to check, no reload happened
    fi
    NEEDLE_CONFIG_CHECK_COUNTER=0

    # Check global config
    local current_global_mtime
    current_global_mtime=$(_needle_get_config_mtime "$global_config")

    if [[ $current_global_mtime -gt $NEEDLE_GLOBAL_CONFIG_LOADED_AT ]]; then
        reload_needed=true
        reload_sources+=("global")
        _needle_debug "Global config changed (mtime: $NEEDLE_GLOBAL_CONFIG_LOADED_AT -> $current_global_mtime)"
    fi

    # Check workspace config if workspace is set
    if [[ -n "${NEEDLE_WORKSPACE:-}" ]]; then
        ws_config="$NEEDLE_WORKSPACE/.needle.yaml"
        local current_ws_mtime
        current_ws_mtime=$(_needle_get_config_mtime "$ws_config")

        if [[ $current_ws_mtime -gt $NEEDLE_WS_CONFIG_LOADED_AT ]]; then
            reload_needed=true
            reload_sources+=("workspace")
            _needle_debug "Workspace config changed (mtime: $NEEDLE_WS_CONFIG_LOADED_AT -> $current_ws_mtime)"
        fi
    fi

    # Perform reload if needed
    if [[ "$reload_needed" == "true" ]]; then
        _needle_info "Config change detected, reloading: ${reload_sources[*]}"

        # Validate new config before applying
        if ! _needle_validate_hot_reload_config "$global_config" "$ws_config"; then
            _needle_error "Config validation failed, keeping current config"
            return 1
        fi

        # Clear caches and reload
        if declare -f clear_config_cache &>/dev/null; then
            clear_config_cache
        fi

        if declare -f clear_workspace_cache &>/dev/null && [[ -n "${NEEDLE_WORKSPACE:-}" ]]; then
            clear_workspace_cache "$NEEDLE_WORKSPACE"
        fi

        # Update tracking timestamps
        NEEDLE_GLOBAL_CONFIG_LOADED_AT=$current_global_mtime
        if [[ -n "$ws_config" ]] && [[ -f "$ws_config" ]]; then
            NEEDLE_WS_CONFIG_LOADED_AT=$current_ws_mtime
        fi

        # Emit telemetry event
        _needle_telemetry_emit "config.reloaded" "info" \
            "sources=${reload_sources[*]}" \
            "global_mtime=$current_global_mtime" \
            "workspace_mtime=${current_ws_mtime:-0}" \
            "session=$NEEDLE_SESSION" \
            "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

        _needle_success "Configuration reloaded successfully"
        return 0  # Reload happened
    fi

    return 1  # No reload needed
}

# Validate configuration before hot-reload
# Returns: 0 if valid, 1 if invalid
# Usage: _needle_validate_hot_reload_config <global_config> [workspace_config]
_needle_validate_hot_reload_config() {
    local global_config="$1"
    local ws_config="${2:-}"

    # Validate global config if it exists
    if [[ -f "$global_config" ]]; then
        # Check YAML syntax with yq if available
        if command -v yq &>/dev/null; then
            if ! yq eval '.' "$global_config" &>/dev/null; then
                _needle_error "Invalid YAML syntax in global config: $global_config"
                return 1
            fi
            # Check that it parsed to something other than null
            local parsed
            parsed=$(yq eval '.' "$global_config" 2>/dev/null)
            if [[ -z "$parsed" ]] || [[ "$parsed" == "null" ]]; then
                _needle_error "Global config file is empty or null: $global_config"
                return 1
            fi
        else
            # Check file is not empty (has non-whitespace content)
            if ! grep -q '[^[:space:]]' "$global_config" 2>/dev/null; then
                _needle_error "Global config file is empty or whitespace only: $global_config"
                return 1
            fi
        fi
    fi

    # Validate workspace config if it exists
    if [[ -n "$ws_config" ]] && [[ -f "$ws_config" ]]; then
        # Check YAML syntax with yq if available
        if command -v yq &>/dev/null; then
            if ! yq eval '.' "$ws_config" &>/dev/null; then
                _needle_error "Invalid YAML syntax in workspace config: $ws_config"
                return 1
            fi
        fi
    fi

    _needle_debug "Config validation passed"
    return 0
}

# Create human alert for crash loop
# Usage: _needle_alert_crash_loop <workspace> <agent>
# Returns: 0 on success, 1 on failure
_needle_alert_crash_loop() {
    local workspace="$1"
    local agent="$2"

    _needle_error "Worker in crash loop: $NEEDLE_FAILURE_COUNT consecutive failures"

    # Emit crash loop event
    _needle_telemetry_emit "worker.crash_loop" "error" \
        "failure_count=$NEEDLE_FAILURE_COUNT" \
        "session=$NEEDLE_SESSION" \
        "workspace=$workspace" \
        "agent=$agent"

    # Use knot strand to create human alert
    local title="NEEDLE Worker Crash Loop: $NEEDLE_SESSION"
    local description
    description=$(cat << EOF
## Worker Crash Loop Detected

The NEEDLE worker has experienced **$NEEDLE_FAILURE_COUNT consecutive failures** and is unable to process beads.

### Context
- **Session:** $NEEDLE_SESSION
- **Runner:** ${NEEDLE_RUNNER:-unknown}
- **Provider:** ${NEEDLE_PROVIDER:-unknown}
- **Model:** ${NEEDLE_MODEL:-unknown}
- **Identifier:** ${NEEDLE_IDENTIFIER:-unknown}
- **Workspace:** $workspace
- **Agent:** $agent
- **Last Failure:** ${NEEDLE_LAST_FAILURE_TIME:-unknown}

### Recommended Actions

1. **Check logs** - Review recent worker logs for error patterns
2. **Verify br CLI** - Ensure br command is working correctly
3. **Check workspace** - Verify workspace is accessible and valid
4. **Restart worker** - May need manual intervention to clear state

### Telemetry
- Failure count: $NEEDLE_FAILURE_COUNT
- Backoff seconds: $NEEDLE_BACKOFF_SECONDS
- Alert threshold: $NEEDLE_ALERT_THRESHOLD
- Max failures: $NEEDLE_MAX_FAILURES

---

*This is an automated alert from NEEDLE Worker Recovery*
*Required for: $workspace*
EOF
)

    # Create human bead for alert using wrapper
    # Note: Human-type beads keep their assignment for visibility
    local bead_id
    bead_id=$(_needle_create_bead \
        --workspace "$workspace" \
        --title "$title" \
        --type human \
        --priority 0 \
        --labels "alert,crash-loop,worker-failure" \
        --description "$description" \
        --silent 2>/dev/null)

    if [[ $? -eq 0 ]] && [[ -n "$bead_id" ]]; then
        _needle_success "Created crash loop alert bead: $bead_id"
        return 0
    fi

    _needle_warn "Failed to create crash loop alert bead"
    return 1
}

# ============================================================================
# Exit Code Handler
# ============================================================================

# Handle execution exit code with appropriate actions
# Usage: _needle_handle_exit_code <bead_id> <exit_code> <workspace> <agent>
# Arguments:
#   bead_id   - The bead being processed
#   exit_code - The exit code from agent execution
#   workspace - The workspace path
#   agent     - The agent name
# Return values:
#   0 - Bead handled successfully
#   1 - Bead handling failed
#   2 - Worker should exit (crash loop)
_needle_handle_exit_code() {
    local bead_id="$1"
    local exit_code="$2"
    local workspace="$3"
    local agent="$4"

    _needle_debug "Handling exit code $exit_code for bead $bead_id"

    case $exit_code in
        0)
            # Success - close bead and reset backoff
            _needle_debug "Exit code 0: Success - closing bead"

            if _needle_complete_bead "$bead_id"; then
                _needle_reset_backoff

                # Emit success event
                _needle_event_bead_completed "$bead_id"
                _needle_telemetry_emit "bead.completed" "info" \
                    "bead_id=$bead_id" \
                    "exit_code=$exit_code" \
                    "session=$NEEDLE_SESSION"

                return 0
            else
                _needle_error "Failed to complete bead: $bead_id"
                return 1
            fi
            ;;

        1)
            # Failure - release and retry later
            _needle_warn "Exit code 1: Failure - releasing bead for retry"

            _needle_release_bead "$bead_id" "agent_failed"
            _needle_increment_backoff

            # Emit failure event
            _needle_event_bead_failed "$bead_id" "reason=agent_failed"
            _needle_telemetry_emit "bead.failed" "error" \
                "bead_id=$bead_id" \
                "exit_code=$exit_code" \
                "failure_count=$NEEDLE_FAILURE_COUNT" \
                "session=$NEEDLE_SESSION"

            # Apply backoff if needed
            _needle_apply_backoff

            # Check for crash loop
            if _needle_should_alert_human; then
                _needle_alert_crash_loop "$workspace" "$agent"
            fi

            if _needle_should_exit_worker; then
                _needle_error "Max failures reached ($NEEDLE_MAX_FAILURES) - exiting worker"
                return 2
            fi

            return 0
            ;;

        124)
            # Timeout - release and flag
            _needle_warn "Exit code 124: Timeout - releasing bead with timeout label"

            # Release with timeout label
            if br update "$bead_id" --release --label "timeout" 2>/dev/null; then
                _needle_increment_backoff

                # Emit timeout event
                _needle_telemetry_emit "bead.timeout" "error" \
                    "bead_id=$bead_id" \
                    "exit_code=$exit_code" \
                    "failure_count=$NEEDLE_FAILURE_COUNT" \
                    "session=$NEEDLE_SESSION"

                _needle_event_bead_released "$bead_id" "reason=timeout"

                # Apply backoff
                _needle_apply_backoff

                return 0
            else
                _needle_error "Failed to release timed out bead: $bead_id"
                return 1
            fi
            ;;

        *)
            # Unknown error - release
            _needle_warn "Exit code $exit_code: Unknown error - releasing bead"

            _needle_release_bead "$bead_id" "unknown_error:$exit_code"
            _needle_increment_backoff

            # Emit error event
            _needle_event_bead_failed "$bead_id" "reason=unknown_error"
            _needle_telemetry_emit "bead.error" "error" \
                "bead_id=$bead_id" \
                "exit_code=$exit_code" \
                "failure_count=$NEEDLE_FAILURE_COUNT" \
                "session=$NEEDLE_SESSION"

            # Apply backoff
            _needle_apply_backoff

            # Check for crash loop
            if _needle_should_alert_human; then
                _needle_alert_crash_loop "$workspace" "$agent"
            fi

            if _needle_should_exit_worker; then
                _needle_error "Max failures reached ($NEEDLE_MAX_FAILURES) - exiting worker"
                return 2
            fi

            return 0
            ;;
    esac
}

# ============================================================================
# Cleanup Functions
# ============================================================================

# Clean up execution context after bead processing
# Usage: _needle_cleanup_execution <bead_id>
_needle_cleanup_execution() {
    local bead_id="$1"

    _needle_debug "Cleaning up execution context for bead: $bead_id"

    # Clear environment variables set during execution
    unset NEEDLE_EXIT_CODE
    unset NEEDLE_DURATION_MS
    unset NEEDLE_OUTPUT_FILE
    unset NEEDLE_CURRENT_BEAD

    # End heartbeat tracking for this bead
    _needle_heartbeat_end_bead

    # Clear any temp files
    if [[ -n "${NEEDLE_TEMP_DIR:-}" ]] && [[ -d "$NEEDLE_TEMP_DIR" ]]; then
        rm -rf "${NEEDLE_TEMP_DIR:?}"/*  2>/dev/null || true
    fi

    _needle_debug "Execution context cleaned up"
}

# Full cleanup on worker shutdown
# Usage: _needle_worker_cleanup
_needle_worker_cleanup() {
    _needle_debug "Performing full worker cleanup..."

    # Clean up heartbeat
    _needle_heartbeat_cleanup

    # Unregister worker from state
    _needle_unregister_worker "$NEEDLE_SESSION"

    # Clear any temp directories
    if [[ -n "${NEEDLE_TEMP_DIR:-}" ]] && [[ -d "$NEEDLE_TEMP_DIR" ]]; then
        rm -rf "$NEEDLE_TEMP_DIR" 2>/dev/null || true
    fi

    # Emit final telemetry
    _needle_telemetry_emit "worker.cleanup" "info" \
        "session=$NEEDLE_SESSION" \
        "failure_count=$NEEDLE_FAILURE_COUNT"

    _needle_debug "Worker cleanup complete"
}

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
    _needle_telemetry_emit "worker.shutdown_initiated" "info" \
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

# Get configuration value with workspace override support
# Uses workspace .needle.yaml override when NEEDLE_WORKSPACE is set
# Usage: _needle_loop_get_config <key> <default>
_needle_loop_get_config() {
    local key="$1"
    local default="$2"

    # Use workspace-aware config lookup when workspace is set
    if [[ -n "${NEEDLE_WORKSPACE:-}" ]] && declare -f get_workspace_config &>/dev/null; then
        get_workspace_config "$NEEDLE_WORKSPACE" "$key" "$default"
    elif declare -f get_config &>/dev/null; then
        get_config "$key" "$default"
    else
        echo "$default"
    fi
}

# Apply workspace config overrides to runtime variables
# Called after hot-reload to update polling interval, idle timeout, etc.
# Usage: _needle_apply_workspace_config_overrides
_needle_apply_workspace_config_overrides() {
    # Re-read runtime configuration values that may have changed
    local new_polling_interval
    new_polling_interval=$(_needle_loop_get_config "runner.polling_interval" "$NEEDLE_LOOP_DEFAULT_POLLING_INTERVAL")
    new_polling_interval="${new_polling_interval%s}"

    local new_idle_timeout
    new_idle_timeout=$(_needle_loop_get_config "runner.idle_timeout" "$NEEDLE_LOOP_DEFAULT_IDLE_TIMEOUT")
    new_idle_timeout="${new_idle_timeout%s}"

    local new_max_consecutive_empty
    new_max_consecutive_empty=$(_needle_loop_get_config "runner.max_consecutive_empty" "$NEEDLE_LOOP_DEFAULT_MAX_EMPTY")

    # Export updated values via global variables for the loop to pick up
    NEEDLE_LOOP_CURRENT_POLLING_INTERVAL="$new_polling_interval"
    NEEDLE_LOOP_CURRENT_IDLE_TIMEOUT="$new_idle_timeout"
    NEEDLE_LOOP_CURRENT_MAX_CONSECUTIVE_EMPTY="$new_max_consecutive_empty"

    _needle_debug "Applied workspace config overrides: polling_interval=${new_polling_interval}s, idle_timeout=${new_idle_timeout}s"
}

# ============================================================================
# Preferred Agents Selection
# ============================================================================

# Current effective agent (may be overridden by preferred_agents)
NEEDLE_LOOP_CURRENT_AGENT=""

# Select the best agent for a workspace based on preferred_agents configuration
# If preferred_agents is configured in .needle.yaml, uses first available agent
# Otherwise, falls back to the provided default agent
#
# Usage: _needle_select_agent_for_workspace <workspace> <default_agent>
# Returns: Selected agent name
_needle_select_agent_for_workspace() {
    local workspace="$1"
    local default_agent="$2"

    # Check if preferred_agents functions are available
    if ! declare -f get_first_available_preferred_agent &>/dev/null; then
        echo "$default_agent"
        return 0
    fi

    # Check if preferred_agents is configured for this workspace
    if ! declare -f has_preferred_agents &>/dev/null; then
        echo "$default_agent"
        return 0
    fi

    # Get first available preferred agent, or fall back to default
    local selected_agent
    selected_agent=$(get_first_available_preferred_agent "$workspace" "$default_agent")

    if [[ -n "$selected_agent" ]] && [[ "$selected_agent" != "$default_agent" ]]; then
        _needle_info "Using preferred agent: $selected_agent (default was: $default_agent)"
    fi

    echo "${selected_agent:-$default_agent}"
}

# Update the current agent based on preferred_agents configuration
# Called on initial load and after hot-reload
# Usage: _needle_update_preferred_agent <workspace> <default_agent>
# Sets: NEEDLE_LOOP_CURRENT_AGENT
_needle_update_preferred_agent() {
    local workspace="$1"
    local default_agent="$2"

    NEEDLE_LOOP_CURRENT_AGENT=$(_needle_select_agent_for_workspace "$workspace" "$default_agent")
}

# ============================================================================
# Strand Engine Integration
# ============================================================================

# Source the strand engine (implements the 7-strand priority waterfall)
# This provides _needle_strand_engine() which dispatches through strands 1-7
source "$NEEDLE_SRC/strands/engine.sh"

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

    # Initialize config change tracking timestamps (required before hot-reload checks)
    _needle_init_config_tracking

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
    _needle_telemetry_emit "worker.started" "info" \
        "workspace=$workspace" \
        "agent=$agent" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    local consecutive_empty=0
    local idle_start=""

    # Initialize runtime config variables (workspace-aware)
    # These are updated on hot-reload via _needle_apply_workspace_config_overrides
    _needle_apply_workspace_config_overrides

    # Initialize preferred agent selection (workspace-aware)
    # This may override the agent based on preferred_agents config
    _needle_update_preferred_agent "$workspace" "$agent"

    # Use mutable references to runtime config (updated on hot-reload)
    local polling_interval="$NEEDLE_LOOP_CURRENT_POLLING_INTERVAL"
    local idle_timeout="$NEEDLE_LOOP_CURRENT_IDLE_TIMEOUT"
    local max_consecutive_empty="$NEEDLE_LOOP_CURRENT_MAX_CONSECUTIVE_EMPTY"

    _needle_debug "Configuration: polling_interval=${polling_interval}s, idle_timeout=${idle_timeout}s"
    _needle_debug "Starting worker loop for workspace: $workspace, agent: $NEEDLE_LOOP_CURRENT_AGENT"

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

        # Check for priority bump signals from waiting beads
        if declare -f _needle_check_priority_bumps &>/dev/null; then
            local bump_signals
            bump_signals=$(_needle_check_priority_bumps "$NEEDLE_BEAD_ID" 2>/dev/null || echo "[]")
            local bump_count
            bump_count=$(echo "$bump_signals" | jq 'length' 2>/dev/null || echo "0")

            if [[ "$bump_count" -gt 0 ]]; then
                _needle_debug "Detected $bump_count priority bump signal(s)"

                # Handle each priority bump signal
                if declare -f handle_priority_bump &>/dev/null; then
                    while IFS= read -r signal; do
                        [[ -n "$signal" ]] && handle_priority_bump "$signal"
                    done < <(echo "$bump_signals" | jq -c '.[]' 2>/dev/null)
                fi
            fi
        fi

        # Check for config hot-reload (every N iterations)
        # If config reloaded (returns 0), apply workspace overrides and update local vars
        if _needle_check_config_reload; then
            _needle_apply_workspace_config_overrides
            # Also update preferred agent (may change if workspace config updated)
            _needle_update_preferred_agent "$workspace" "$agent"
            polling_interval="$NEEDLE_LOOP_CURRENT_POLLING_INTERVAL"
            idle_timeout="$NEEDLE_LOOP_CURRENT_IDLE_TIMEOUT"
            max_consecutive_empty="$NEEDLE_LOOP_CURRENT_MAX_CONSECUTIVE_EMPTY"
            _needle_debug "Runtime config updated: polling_interval=${polling_interval}s, idle_timeout=${idle_timeout}s"
        fi

        # Run strand engine to find work
        local strand_result
        # DIAGNOSTIC: Log strand engine call
        _needle_debug "DIAG: Calling strand engine - consecutive_empty=$consecutive_empty"
        if _needle_strand_engine "$workspace" "$NEEDLE_LOOP_CURRENT_AGENT"; then
            strand_result=$?
        else
            strand_result=$?
        fi
        _needle_debug "DIAG: Strand engine returned: $strand_result"

        if [[ $strand_result -eq 0 ]]; then
            # Work found and processed
            consecutive_empty=0
            idle_start=""
            _needle_debug "Strand engine found work"
        elif [[ "$strand_result" -eq 1 ]]; then
            # No work found
            ((consecutive_empty++)) || true

            # Track idle time
            if [[ -z "$idle_start" ]]; then
                idle_start=$(date +%s)
            fi

            local idle_seconds=$(($(date +%s) - idle_start))

            _needle_event_worker_idle \
                "consecutive_empty=$consecutive_empty" \
                "idle_seconds=$idle_seconds" \
                "workspace=$workspace" \
                "agent=$NEEDLE_LOOP_CURRENT_AGENT"

            _needle_debug "Worker idle: consecutive_empty=$consecutive_empty, idle_seconds=${idle_seconds}s"

            # Check idle timeout
            if ((idle_seconds >= idle_timeout)); then
                _needle_debug "Idle timeout reached (${idle_seconds}s >= ${idle_timeout}s)"
                _needle_event_worker_stopped "reason=idle_timeout"
                _needle_telemetry_emit "worker.stopped" "info" \
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
    _needle_telemetry_emit "worker.stopped" "info" \
        "reason=shutdown" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "failure_count=$NEEDLE_FAILURE_COUNT"

    # Perform full cleanup
    _needle_worker_cleanup

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
    _needle_telemetry_emit "bead.claimed" "info" \
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
    _needle_telemetry_emit "bead.prompt_built" "info" \
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
    _needle_telemetry_emit "bead.agent_started" "info" \
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
    _needle_telemetry_emit "bead.agent_completed" "info" \
        "bead_id=$bead_id" \
        "exit_code=$dispatch_exit" \
        "duration_ms=$dispatch_duration" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    # ========================================================================
    # Effort Recording (Token Extraction + Cost Calculation)
    # ========================================================================

    # Extract tokens from output file
    local input_tokens=0
    local output_tokens=0
    local cost="0.00"

    if [[ -n "$dispatch_output" ]] && [[ -f "$dispatch_output" ]]; then
        local token_result
        token_result=$(_needle_extract_tokens "$agent" "$dispatch_output")

        if [[ -n "$token_result" ]]; then
            input_tokens=$(echo "$token_result" | cut -d'|' -f1)
            output_tokens=$(echo "$token_result" | cut -d'|' -f2)

            # Ensure we have valid numbers
            [[ ! "$input_tokens" =~ ^[0-9]+$ ]] && input_tokens=0
            [[ ! "$output_tokens" =~ ^[0-9]+$ ]] && output_tokens=0

            _needle_debug "Extracted tokens: input=$input_tokens, output=$output_tokens"
        fi

        # Calculate cost
        cost=$(calculate_cost "$agent" "$input_tokens" "$output_tokens")
        _needle_debug "Calculated cost: \$$cost"
    fi

    # Record effort for this bead
    if [[ "$input_tokens" -gt 0 ]] || [[ "$output_tokens" -gt 0 ]]; then
        record_effort "$bead_id" "$cost" "$agent" "$input_tokens" "$output_tokens"
        _needle_debug "Recorded effort: bead=$bead_id, cost=\$$cost, agent=$agent"
    fi

    # Emit effort recorded telemetry
    _needle_telemetry_emit "bead.effort_recorded" "info" \
        "bead_id=$bead_id" \
        "agent=$agent" \
        "input_tokens=$input_tokens" \
        "output_tokens=$output_tokens" \
        "cost=$cost" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    # Export effort data for post-execute hook
    export NEEDLE_INPUT_TOKENS="$input_tokens"
    export NEEDLE_OUTPUT_TOKENS="$output_tokens"
    export NEEDLE_COST="$cost"

    # Run post-execute hook
    # Set environment variables for the hook
    export NEEDLE_EXIT_CODE="$dispatch_exit"
    export NEEDLE_DURATION_MS="$dispatch_duration"
    export NEEDLE_OUTPUT_FILE="$dispatch_output"
    export NEEDLE_CURRENT_BEAD="$bead_id"

    if ! _needle_run_hook "post_execute" "$bead_id"; then
        _needle_warn "Post-execute hook failed for bead $bead_id"
    fi

    # Handle result using exit code handler
    # This implements proper cleanup, backoff, and crash recovery
    local handle_result
    _needle_handle_exit_code "$bead_id" "$dispatch_exit" "$workspace" "$agent"
    handle_result=$?

    # Cleanup execution context
    _needle_cleanup_execution "$bead_id"

    # Cleanup output file
    if [[ -n "$dispatch_output" ]] && [[ -f "$dispatch_output" ]]; then
        rm -f "$dispatch_output"
    fi

    # Check if worker should exit due to crash loop
    if [[ $handle_result -eq 2 ]]; then
        _needle_error "Worker exiting due to crash loop"
        _needle_worker_cleanup
        return 2
    fi

    _needle_debug "Completed processing bead: $bead_id"
    return $handle_result
}

# ============================================================================
# Bead Completion Functions
# ============================================================================

# Complete a bead successfully
# Usage: _needle_complete_bead <bead_id> [output_file]
# Return values:
#   0 - Bead completed successfully
#   1 - Completion failed (quality gate, hook abort, or br error)
_needle_complete_bead() {
    local bead_id="$1"
    local output_file="${2:-}"

    _needle_debug "Completing bead: $bead_id"

    # Load bug scanner configuration
    local bs_enabled bs_severity bs_fail_on_issues bs_timeout
    bs_enabled=$(get_config "bug_scanner.enabled" "true")
    bs_severity=$(get_config "bug_scanner.severity_threshold" "error")
    bs_fail_on_issues=$(get_config "bug_scanner.fail_on_issues" "true")
    bs_timeout=$(get_config "bug_scanner.timeout" "300")

    # Export config for bug_scanner module
    export BUG_SCANNER_ENABLED="$bs_enabled"
    export BUG_SCANNER_SEVERITY_THRESHOLD="$bs_severity"
    export BUG_SCANNER_FAIL_ON_ISSUES="$bs_fail_on_issues"
    export BUG_SCANNER_TIMEOUT="$bs_timeout"

    # Run bug scanner quality gate (before hooks)
    if [[ "$bs_enabled" == "true" ]] && declare -f bug_scanner_scan_bead &>/dev/null; then
        local scan_dir="${NEEDLE_WORKSPACE:-$(pwd)}"
        _needle_info "Running bug scanner quality gate on $scan_dir"

        if ! bug_scanner_scan_bead "$bead_id" "$scan_dir"; then
            _needle_warn "Bug scanner found issues exceeding threshold '$bs_severity'"
            if [[ "$bs_fail_on_issues" == "true" ]]; then
                _needle_error "Failing bead due to bug scan results"
                _needle_fail_bead "$bead_id" "bug_scan_failed"
                return 1
            else
                _needle_warn "Continuing despite bug scan issues (fail_on_issues=false)"
            fi
        fi
    fi

    # Run pre-complete hook (additional quality gates)
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
    _needle_telemetry_emit "bead.completed" "info" \
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
    _needle_telemetry_emit "bead.failed" "error" \
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
# Bead Quarantine Function
# ============================================================================

# Quarantine a bead (permanently close, not retryable)
# Usage: _needle_quarantine_bead <bead_id> [reason]
# Return values:
#   0 - Bead quarantined successfully
#   1 - Quarantine failed
_needle_quarantine_bead() {
    local bead_id="$1"
    local reason="${2:-quarantined}"

    _needle_debug "Quarantining bead: $bead_id (reason: $reason)"

    # Close with quarantined label so status dashboard can count it
    if ! br update "$bead_id" --status closed --add-label "quarantined" 2>/dev/null; then
        _needle_error "Failed to quarantine bead: $bead_id"
        _needle_event_error_complete_failed "$bead_id" "reason=quarantine_command_failed"
        # Still run on_quarantine hook for alerting
        _needle_run_hook "on_quarantine" "$bead_id"
        return 1
    fi

    # Emit quarantined event
    _needle_telemetry_emit "bead.quarantined" "error" \
        "bead_id=$bead_id" \
        "reason=$reason" \
        "session=$NEEDLE_SESSION" \
        "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    _needle_warn "Bead quarantined: $bead_id (reason: $reason)"

    # Run on_quarantine hook for alerting/escalation
    _needle_run_hook "on_quarantine" "$bead_id"

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
    _needle_telemetry_emit "bead.released" "info" \
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
