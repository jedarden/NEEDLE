#!/usr/bin/env bash
# NEEDLE Agent Dispatcher Module
# Renders invoke templates and executes agents
#
# This module implements the core execution engine that:
# 1. Loads agent configuration
# 2. Renders invoke templates with variables
# 3. Executes via bash with appropriate input method
# 4. Captures output and exit code
# 5. Measures execution duration
# 6. Handles timeouts and signals

# Source dependencies (if not already loaded)
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

# Source bug scanner module for pre-flight checks (if available)
if [[ -f "$(dirname "${BASH_SOURCE[0]}")/../quality/bug_scanner.sh" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../quality/bug_scanner.sh"
fi

# Source FABRIC telemetry module (if available)
if [[ -f "$(dirname "${BASH_SOURCE[0]}")/../telemetry/fabric.sh" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../telemetry/fabric.sh"
fi

# Module state
_NEEDLE_DISPATCH_PID=""
_NEEDLE_DISPATCH_OUTPUT_FILE=""
_NEEDLE_DISPATCH_START_TIME=0
_NEEDLE_HEARTBEAT_BG_PID=""
_NEEDLE_FABRIC_PIPE=""
_NEEDLE_FABRIC_PID=""

# -----------------------------------------------------------------------------
# Background Heartbeat Process
# -----------------------------------------------------------------------------

# Start background heartbeat process during agent execution
# This keeps the worker alive during long-running agent tasks
# Usage: _needle_start_heartbeat_background <bead_id>
_needle_start_heartbeat_background() {
    local bead_id="$1"
    local interval="${NEEDLE_HEARTBEAT_INTERVAL:-30}"

    # Start background process that emits heartbeats
    # Redirect stdout to stderr so heartbeat JSON doesn't pollute
    # the dispatch_result captured via $() subshells
    (
        while true; do
            # Check if heartbeat functions are available
            if declare -f _needle_heartbeat_keepalive &>/dev/null; then
                _needle_heartbeat_keepalive
            elif declare -f _needle_emit_heartbeat &>/dev/null; then
                _needle_emit_heartbeat "executing" "$bead_id"
            fi
            sleep "$interval"
        done
    ) >&2 &

    _NEEDLE_HEARTBEAT_BG_PID=$!
    _needle_debug "Started background heartbeat process: PID $_NEEDLE_HEARTBEAT_BG_PID (interval: ${interval}s)"
}

# Stop background heartbeat process
# Usage: _needle_stop_heartbeat_background
_needle_stop_heartbeat_background() {
    if [[ -n "$_NEEDLE_HEARTBEAT_BG_PID" ]]; then
        if kill -0 "$_NEEDLE_HEARTBEAT_BG_PID" 2>/dev/null; then
            kill "$_NEEDLE_HEARTBEAT_BG_PID" 2>/dev/null
            wait "$_NEEDLE_HEARTBEAT_BG_PID" 2>/dev/null
            _needle_debug "Stopped background heartbeat process: PID $_NEEDLE_HEARTBEAT_BG_PID"
        fi
        _NEEDLE_HEARTBEAT_BG_PID=""
    fi
}

# -----------------------------------------------------------------------------
# FABRIC Event Forwarding
# -----------------------------------------------------------------------------

# Start FABRIC event forwarder for stream-json output
# Creates a named pipe and background process to forward events
# Usage: _needle_start_fabric_forwarder
# Returns: 0 on success, 1 if not enabled or failed
_needle_start_fabric_forwarder() {
    # Check if FABRIC module is loaded and enabled
    if ! declare -f _needle_fabric_is_enabled &>/dev/null; then
        return 1
    fi

    if ! _needle_fabric_is_enabled; then
        return 1
    fi

    # Create named pipe for event forwarding
    local pipe_path
    if declare -f _needle_fabric_create_pipe &>/dev/null; then
        pipe_path=$(_needle_fabric_create_pipe)
        if [[ -z "$pipe_path" ]]; then
            _needle_warn "Failed to create FABRIC pipe, forwarding disabled"
            return 1
        fi
    else
        return 1
    fi

    _NEEDLE_FABRIC_PIPE="$pipe_path"

    # Start forwarder process
    if declare -f _needle_fabric_start_forwarder &>/dev/null; then
        local forwarder_pid
        forwarder_pid=$(_needle_fabric_start_forwarder "$pipe_path")
        if [[ -n "$forwarder_pid" ]]; then
            _NEEDLE_FABRIC_PID="$forwarder_pid"
            _needle_debug "Started FABRIC forwarder: PID $forwarder_pid, pipe $pipe_path"
            return 0
        fi
    fi

    # Clean up pipe if forwarder failed to start
    rm -f "$pipe_path" 2>/dev/null
    _NEEDLE_FABRIC_PIPE=""
    return 1
}

# Stop FABRIC event forwarder
# Usage: _needle_stop_fabric_forwarder
_needle_stop_fabric_forwarder() {
    if declare -f _needle_fabric_stop_forwarder &>/dev/null; then
        _needle_fabric_stop_forwarder "$_NEEDLE_FABRIC_PID" "$_NEEDLE_FABRIC_PIPE"
        _needle_debug "Stopped FABRIC forwarder"
    fi

    _NEEDLE_FABRIC_PID=""
    _NEEDLE_FABRIC_PIPE=""
}

# Run tee for output redirection
# If FABRIC is enabled, tee to both output file and FABRIC pipe
# Otherwise, just tee to output file
# Usage: _needle_run_tee <output_file>
_needle_run_tee() {
    local output_file="$1"

    if [[ -n "$_NEEDLE_FABRIC_PIPE" ]] && [[ -p "$_NEEDLE_FABRIC_PIPE" ]]; then
        # Tee to both output file and FABRIC pipe (output visible in terminal)
        tee "$output_file" "$_NEEDLE_FABRIC_PIPE"
    else
        # Tee to output file (output visible in terminal)
        tee "$output_file"
    fi
}

# -----------------------------------------------------------------------------
# Template Rendering
# -----------------------------------------------------------------------------

# Render invoke template with variable substitution
# Variables: ${WORKSPACE}, ${PROMPT}, ${BEAD_ID}, ${BEAD_TITLE}, ${AGENT_DIR}
#
# Usage: _needle_render_invoke <template> <workspace> <prompt> <bead_id> <bead_title>
# Returns: Rendered template string
_needle_render_invoke() {
    local template="$1"
    local workspace="$2"
    local prompt="$3"
    local bead_id="$4"
    local bead_title="$5"

    # Start with the template
    local rendered="$template"

    # Replace variables in order (most specific first)
    # Note: We use pattern replacement which handles multi-line strings correctly

    # Escape special characters in replacement values for bash safety
    # For heredoc templates, the PROMPT is inserted literally inside the heredoc

    # Replace ${WORKSPACE}
    rendered="${rendered//\$\{WORKSPACE\}/$workspace}"

    # Replace ${BEAD_ID}
    rendered="${rendered//\$\{BEAD_ID\}/$bead_id}"

    # Replace ${BEAD_TITLE}
    rendered="${rendered//\$\{BEAD_TITLE\}/$bead_title}"

    # Replace ${AGENT_DIR} - directory containing the agent config and stream-parser
    local agent_dir="${NEEDLE_AGENT[agent_dir]:-}"
    rendered="${rendered//\$\{AGENT_DIR\}/$agent_dir}"

    # Replace ${PROMPT} - this is the tricky one
    # In heredoc templates, ${PROMPT} appears inside the heredoc block
    # which is treated as literal text when delimiter is quoted
    rendered="${rendered//\$\{PROMPT\}/$prompt}"

    echo "$rendered"
}

# Render template for args input method (prompts need escaping)
# Usage: _needle_render_invoke_args <template> <workspace> <prompt> <bead_id> <bead_title>
_needle_render_invoke_args() {
    local template="$1"
    local workspace="$2"
    local prompt="$3"
    local bead_id="$4"
    local bead_title="$5"

    # For args method, escape the prompt for double-quote embedding
    local escaped_prompt
    escaped_prompt=$(_needle_escape_prompt_for_args "$prompt")

    # Start with template
    local rendered="$template"

    # Replace variables
    rendered="${rendered//\$\{WORKSPACE\}/$workspace}"
    rendered="${rendered//\$\{BEAD_ID\}/$bead_id}"
    rendered="${rendered//\$\{BEAD_TITLE\}/$bead_title}"

    # Replace ${AGENT_DIR} - directory containing the agent config and stream-parser
    local agent_dir="${NEEDLE_AGENT[agent_dir]:-}"
    rendered="${rendered//\$\{AGENT_DIR\}/$agent_dir}"

    rendered="${rendered//\$\{PROMPT\}/$escaped_prompt}"

    echo "$rendered"
}

# Escape prompt for args-style invocation (double-quoted string)
# Usage: _needle_escape_prompt_for_args <prompt>
_needle_escape_prompt_for_args() {
    local prompt="$1"

    # Escape characters that are special in double-quoted bash strings
    # Order matters: backslash must be first
    local escaped="$prompt"
    escaped="${escaped//\\/\\\\}"      # Backslash -> \\
    escaped="${escaped//\"/\\\"}"      # Double quote -> \"
    escaped="${escaped//\$/\\\$}"      # Dollar sign -> \$
    escaped="${escaped//\`/\\\`}"      # Backtick -> \`

    echo "$escaped"
}

# -----------------------------------------------------------------------------
# Input Method Dispatchers
# -----------------------------------------------------------------------------

# Dispatch using heredoc input method (default for Claude)
# The template already contains the heredoc structure
#
# Usage: _needle_dispatch_heredoc <rendered_template> <output_file>
# Returns: Exit code of the command
_needle_dispatch_heredoc() {
    local rendered="$1"
    local output_file="$2"
    local timeout="${3:-0}"

    _needle_debug "Dispatching with heredoc method"

    if [[ "$timeout" -gt 0 ]]; then
        timeout "$timeout" bash -c "$rendered" 2>&1 | _needle_run_tee "$output_file"
        local exit_code=${PIPESTATUS[0]}
        # timeout returns 124 when timed out
        if [[ $exit_code -eq 124 ]]; then
            _needle_warn "Command timed out after ${timeout}s"
        fi
        return $exit_code
    else
        bash -c "$rendered" 2>&1 | _needle_run_tee "$output_file"
        return ${PIPESTATUS[0]}
    fi
}

# Dispatch using stdin input method
# Pipe the prompt to the command
#
# Usage: _needle_dispatch_stdin <invoke_cmd> <prompt> <output_file> [timeout]
# Returns: Exit code of the command
_needle_dispatch_stdin() {
    local invoke_cmd="$1"
    local prompt="$2"
    local output_file="$3"
    local timeout="${4:-0}"

    _needle_debug "Dispatching with stdin method"

    if [[ "$timeout" -gt 0 ]]; then
        echo "$prompt" | timeout "$timeout" bash -c "$invoke_cmd" 2>&1 | _needle_run_tee "$output_file"
        local exit_code=${PIPESTATUS[1]}
        if [[ $exit_code -eq 124 ]]; then
            _needle_warn "Command timed out after ${timeout}s"
        fi
        return $exit_code
    else
        echo "$prompt" | bash -c "$invoke_cmd" 2>&1 | _needle_run_tee "$output_file"
        return ${PIPESTATUS[1]}
    fi
}

# Dispatch using file input method
# Write prompt to file, then execute command that reads from file
#
# Usage: _needle_dispatch_file <invoke_cmd> <prompt> <file_path> <output_file> [timeout]
# Returns: Exit code of the command
_needle_dispatch_file() {
    local invoke_cmd="$1"
    local prompt="$2"
    local file_path="$3"
    local output_file="$4"
    local timeout="${5:-0}"

    _needle_debug "Dispatching with file method to: $file_path"

    # Write prompt to the input file
    if ! echo "$prompt" > "$file_path" 2>/dev/null; then
        _needle_error "Failed to write prompt file: $file_path"
        return 1
    fi

    # Replace ${PROMPT_FILE} placeholder in command if present
    local resolved_cmd="${invoke_cmd//\$\{PROMPT_FILE\}/$file_path}"

    local exit_code
    if [[ "$timeout" -gt 0 ]]; then
        timeout "$timeout" bash -c "$resolved_cmd" 2>&1 | _needle_run_tee "$output_file"
        exit_code=${PIPESTATUS[0]}
        if [[ $exit_code -eq 124 ]]; then
            _needle_warn "Command timed out after ${timeout}s"
        fi
    else
        bash -c "$resolved_cmd" 2>&1 | _needle_run_tee "$output_file"
        exit_code=${PIPESTATUS[0]}
    fi

    # Clean up the prompt file
    rm -f "$file_path" 2>/dev/null

    return $exit_code
}

# Dispatch using args input method
# Pass prompt as command-line argument
#
# Usage: _needle_dispatch_args <rendered_template> <output_file> [timeout]
# Returns: Exit code of the command
_needle_dispatch_args() {
    local rendered="$1"
    local output_file="$2"
    local timeout="${3:-0}"

    _needle_debug "Dispatching with args method"

    if [[ "$timeout" -gt 0 ]]; then
        timeout "$timeout" bash -c "$rendered" 2>&1 | _needle_run_tee "$output_file"
        local exit_code=${PIPESTATUS[0]}
        if [[ $exit_code -eq 124 ]]; then
            _needle_warn "Command timed out after ${timeout}s"
        fi
        return $exit_code
    else
        bash -c "$rendered" 2>&1 | _needle_run_tee "$output_file"
        return ${PIPESTATUS[0]}
    fi
}

# -----------------------------------------------------------------------------
# Main Dispatcher
# -----------------------------------------------------------------------------

# Dispatch agent to work on a bead
# This is the main entry point for agent execution
#
# Usage: _needle_dispatch_agent <agent_name> <workspace> <prompt> <bead_id> <bead_title> [timeout]
# Returns: Pipe-delimited string: exit_code|duration_ms|output_file
#
# Example:
#   result=$(_needle_dispatch_agent "claude-anthropic-sonnet" "/home/user/project" "Fix the bug" "nd-100" "Fix bug")
#   exit_code=$(echo "$result" | cut -d'|' -f1)
#   duration=$(echo "$result" | cut -d'|' -f2)
#   output_file=$(echo "$result" | cut -d'|' -f3)
_needle_dispatch_agent() {
    local agent_name="$1"
    local workspace="$2"
    local prompt="$3"
    local bead_id="$4"
    local bead_title="$5"
    local timeout="${6:-0}"

    # Validate required parameters
    if [[ -z "$agent_name" ]]; then
        _needle_error "Agent name is required"
        return 1
    fi

    if [[ -z "$workspace" ]]; then
        _needle_error "Workspace is required"
        return 1
    fi

    if [[ -z "$prompt" ]]; then
        _needle_error "Prompt is required"
        return 1
    fi

    # Load agent configuration (uses NEEDLE_AGENT associative array)
    if ! _needle_load_agent "$agent_name"; then
        _needle_error "Failed to load agent: $agent_name"
        return 1
    fi

    _needle_debug "Dispatching agent: ${NEEDLE_AGENT[name]} (${NEEDLE_AGENT[input_method]} method)"
    _needle_verbose "Bead: $bead_id - $bead_title"
    _needle_verbose "Workspace: $workspace"

    # Optional pre-flight bug check (before agent execution)
    # This catches critical issues before expensive agent execution
    local bs_preflight
    if declare -f get_config &>/dev/null; then
        bs_preflight=$(get_config "bug_scanner.preflight_check" "false")
    else
        bs_preflight="${BUG_SCANNER_PREFLIGHT:-false}"
    fi

    if [[ "$bs_preflight" == "true" ]] && declare -f bug_scanner_quick_check &>/dev/null; then
        _needle_info "Running pre-flight bug check on workspace"

        # Load config for scanner
        local bs_severity
        if declare -f get_config &>/dev/null; then
            bs_severity=$(get_config "bug_scanner.severity_threshold" "error")
        else
            bs_severity="${BUG_SCANNER_SEVERITY_THRESHOLD:-error}"
        fi

        export BUG_SCANNER_SEVERITY_THRESHOLD="$bs_severity"

        if ! bug_scanner_quick_check "$workspace"; then
            _needle_warn "Pre-flight check found critical issues in workspace"
            # For pre-flight, we warn but don't abort - let the agent decide
            # The post-execution scan will catch issues again at completion
        fi
    fi

    # Create output capture file
    local output_file
    output_file=$(mktemp "${TMPDIR:-/tmp}/needle-dispatch-${bead_id}-XXXXXXXX.log")
    _NEEDLE_DISPATCH_OUTPUT_FILE="$output_file"

    # Record start time (milliseconds)
    local start_time
    start_time=$(_needle_get_time_ms)
    _NEEDLE_DISPATCH_START_TIME="$start_time"

    # Start background heartbeat to keep worker alive during execution
    _needle_start_heartbeat_background "$bead_id"

    # Export NEEDLE_HEARTBEAT_CMD for stream-parser.sh inline heartbeat emission
    # This is more reliable than the background process since it fires on every
    # JSONL event from the CLI, proving the agent is actively producing output.
    local _hb_script
    _hb_script=$(mktemp "${TMPDIR:-/tmp}/needle-hb-${bead_id}-XXXXXXXX.sh")
    cat > "$_hb_script" <<NEEDLE_HB_EOF
#!/bin/bash
printf '{"worker":"${NEEDLE_SESSION}","pid":$$,"started":"${NEEDLE_HEARTBEAT_STARTED}","last_heartbeat":"%s","status":"executing","current_bead":"${bead_id}","workspace":"${NEEDLE_WORKSPACE}","agent":"${NEEDLE_AGENT[name]:-unknown}","queue_depth":0}' "\$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "${NEEDLE_HEARTBEAT_FILE}"
NEEDLE_HB_EOF
    chmod +x "$_hb_script"
    export NEEDLE_HEARTBEAT_CMD="bash $_hb_script"

    # Start FABRIC event forwarder if enabled and agent uses stream-json
    local output_format="${NEEDLE_AGENT[output_format]:-text}"
    if [[ "$output_format" == "stream-json" ]] || [[ "$output_format" == "streaming" ]]; then
        _needle_start_fabric_forwarder
    fi

    # Wire LD_PRELOAD for non-Claude agents (opencode, aider, etc.)
    # Claude Code has native hook support; other agents need libc-level enforcement.
    local _ld_preload_set=false
    local agent_runner="${NEEDLE_AGENT[runner]:-}"
    if [[ "$agent_runner" != "claude" ]]; then
        local ld_preload_enabled="false"
        if declare -f get_config &>/dev/null; then
            ld_preload_enabled=$(get_config "file_locks.ld_preload" "false")
        fi
        if [[ "$ld_preload_enabled" == "true" ]]; then
            local lib_path="${NEEDLE_HOME:-$HOME/.needle}/lib/libcheckout.so"
            if declare -f get_config &>/dev/null; then
                local cfg_lib
                cfg_lib=$(get_config "file_locks.ld_preload_lib" "")
                [[ -n "$cfg_lib" ]] && lib_path="$cfg_lib"
            fi
            if [[ -f "$lib_path" ]]; then
                export LD_PRELOAD="$lib_path"
                export NEEDLE_BEAD_ID="$bead_id"
                _ld_preload_set=true
                _needle_debug "LD_PRELOAD enabled for non-Claude agent ($agent_runner): $lib_path"
            else
                _needle_warn "file_locks.ld_preload enabled but library not found: $lib_path"
                _needle_warn "Run 'scripts/build-native.sh' to build libcheckout.so"
            fi
        fi
    fi

    # Render template and execute based on input method
    local exit_code
    local input_method="${NEEDLE_AGENT[input_method]:-heredoc}"

    case "$input_method" in
        heredoc)
            # Render template (prompt is embedded in heredoc literally)
            local rendered
            rendered=$(_needle_render_invoke \
                "${NEEDLE_AGENT[invoke]}" \
                "$workspace" \
                "$prompt" \
                "$bead_id" \
                "$bead_title"
            )

            _needle_dispatch_heredoc "$rendered" "$output_file" "$timeout"
            exit_code=$?
            ;;

        stdin)
            # For stdin, invoke template is just the command
            # We pipe the prompt to it
            _needle_dispatch_stdin \
                "${NEEDLE_AGENT[invoke]}" \
                "$prompt" \
                "$output_file" \
                "$timeout"
            exit_code=$?
            ;;

        file)
            # For file method, determine the file path
            local file_path="${NEEDLE_AGENT[input_file_path]:-${TMPDIR:-/tmp}/needle-prompt-${bead_id}.txt}"

            _needle_dispatch_file \
                "${NEEDLE_AGENT[invoke]}" \
                "$prompt" \
                "$file_path" \
                "$output_file" \
                "$timeout"
            exit_code=$?
            ;;

        args)
            # For args method, render with proper escaping
            local rendered
            rendered=$(_needle_render_invoke_args \
                "${NEEDLE_AGENT[invoke]}" \
                "$workspace" \
                "$prompt" \
                "$bead_id" \
                "$bead_title"
            )

            _needle_dispatch_args "$rendered" "$output_file" "$timeout"
            exit_code=$?
            ;;

        *)
            _needle_error "Unknown input method: $input_method"
            _needle_stop_heartbeat_background
            [[ "$_ld_preload_set" == "true" ]] && unset LD_PRELOAD NEEDLE_BEAD_ID
            rm -f "$output_file"
            return 1
            ;;
    esac

    # Stop background heartbeat now that agent has completed
    _needle_stop_heartbeat_background

    # Clean up heartbeat script and unset command
    rm -f "$_hb_script" 2>/dev/null
    unset NEEDLE_HEARTBEAT_CMD

    # Clean up LD_PRELOAD if we set it
    if [[ "$_ld_preload_set" == "true" ]]; then
        unset LD_PRELOAD
        unset NEEDLE_BEAD_ID
    fi

    # Stop FABRIC event forwarder if it was started
    _needle_stop_fabric_forwarder

    # Record end time and calculate duration
    local end_time
    end_time=$(_needle_get_time_ms)
    local duration=$((end_time - start_time))

    _needle_debug "Agent completed: exit_code=$exit_code, duration=${duration}ms"

    # Return results as pipe-delimited string
    echo "${exit_code}|${duration}|${output_file}"
}

# -----------------------------------------------------------------------------
# Utility Functions
# -----------------------------------------------------------------------------

# Get current time in milliseconds
# Usage: _needle_get_time_ms
_needle_get_time_ms() {
    if [[ -f /proc/uptime ]]; then
        # Linux: use /proc/uptime for sub-second precision
        local uptime
        read -r uptime _ < /proc/uptime
        # Convert to integer milliseconds
        echo "${uptime/./}"
    else
        # Fallback: use date (may not have milliseconds on all systems)
        date +%s%3N 2>/dev/null || echo "$(date +%s)000"
    fi
}

# Clean up dispatch resources (call on exit or interrupt)
# Usage: _needle_dispatch_cleanup
_needle_dispatch_cleanup() {
    # Stop background heartbeat process
    _needle_stop_heartbeat_background

    # Stop FABRIC forwarder
    _needle_stop_fabric_forwarder

    # Kill any running process
    if [[ -n "$_NEEDLE_DISPATCH_PID" ]] && kill -0 "$_NEEDLE_DISPATCH_PID" 2>/dev/null; then
        _needle_debug "Killing dispatch process: $_NEEDLE_DISPATCH_PID"
        kill -TERM "$_NEEDLE_DISPATCH_PID" 2>/dev/null
        wait "$_NEEDLE_DISPATCH_PID" 2>/dev/null
    fi

    # Clean up output file if it exists
    if [[ -n "$_NEEDLE_DISPATCH_OUTPUT_FILE" ]] && [[ -f "$_NEEDLE_DISPATCH_OUTPUT_FILE" ]]; then
        rm -f "$_NEEDLE_DISPATCH_OUTPUT_FILE" 2>/dev/null
    fi
}

# Parse dispatch result string
# Usage: _needle_parse_dispatch_result <result_string> <var_prefix>
# Sets: <var_prefix>_exit_code, <var_prefix>_duration, <var_prefix>_output_file
_needle_parse_dispatch_result() {
    local result="$1"
    local prefix="$2"

    if [[ -z "$result" ]]; then
        return 1
    fi

    IFS='|' read -r ${prefix}_exit_code ${prefix}_duration ${prefix}_output_file <<< "$result"
}

# Check if exit code indicates success
# Uses agent's success_codes configuration
# Usage: _needle_is_success_exit_code <exit_code>
_needle_is_success_exit_code() {
    local exit_code="$1"
    local success_codes="${NEEDLE_AGENT[success_codes]:-0}"

    # Check if exit code is in success codes
    for code in $success_codes; do
        if [[ "$code" == "$exit_code" ]]; then
            return 0
        fi
    done

    return 1
}

# Check if exit code indicates retryable error
# Uses agent's retry_codes configuration
# Usage: _needle_is_retry_exit_code <exit_code>
_needle_is_retry_exit_code() {
    local exit_code="$1"
    local retry_codes="${NEEDLE_AGENT[retry_codes]:-1}"

    for code in $retry_codes; do
        if [[ "$code" == "$exit_code" ]]; then
            return 0
        fi
    done

    return 1
}

# Check if exit code indicates hard failure
# Uses agent's fail_codes configuration
# Usage: _needle_is_fail_exit_code <exit_code>
_needle_is_fail_exit_code() {
    local exit_code="$1"
    local fail_codes="${NEEDLE_AGENT[fail_codes]:-2 137}"

    for code in $fail_codes; do
        if [[ "$code" == "$exit_code" ]]; then
            return 0
        fi
    done

    return 1
}

# Classify exit code as success/retry/fail
# Usage: _needle_classify_exit_code <exit_code>
# Returns: "success", "retry", or "fail"
_needle_classify_exit_code() {
    local exit_code="$1"

    if _needle_is_success_exit_code "$exit_code"; then
        echo "success"
    elif _needle_is_retry_exit_code "$exit_code"; then
        echo "retry"
    else
        echo "fail"
    fi
}

# -----------------------------------------------------------------------------
# Signal Handling
# -----------------------------------------------------------------------------

# Set up signal handlers for clean termination
# Usage: _needle_setup_signal_handlers
_needle_setup_signal_handlers() {
    trap '_needle_handle_signal SIGTERM' TERM
    trap '_needle_handle_signal SIGINT' INT
    trap '_needle_handle_signal SIGHUP' HUP
}

# Handle termination signals
# Usage: _needle_handle_signal <signal_name>
_needle_handle_signal() {
    local signal="$1"
    _needle_warn "Received $signal, cleaning up..."

    _needle_dispatch_cleanup

    exit 130  # 128 + signal number (2 for SIGINT)
}

# -----------------------------------------------------------------------------
# Direct Execution Support (for testing)
# -----------------------------------------------------------------------------

# Allow running this module directly for testing
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    # Source dependencies for direct execution
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
    source "$(dirname "${BASH_SOURCE[0]}")/loader.sh"
    source "$(dirname "${BASH_SOURCE[0]}")/escape.sh"

    case "${1:-}" in
        render)
            if [[ $# -lt 5 ]]; then
                echo "Usage: $0 render <template> <workspace> <prompt> <bead_id> <bead_title>"
                exit 1
            fi
            _needle_render_invoke "$2" "$3" "$4" "$5" "$6"
            ;;
        dispatch)
            if [[ $# -lt 5 ]]; then
                echo "Usage: $0 dispatch <agent_name> <workspace> <prompt> <bead_id> <bead_title> [timeout]"
                exit 1
            fi
            _needle_dispatch_agent "$2" "$3" "$4" "$5" "$6" "${7:-0}"
            ;;
        escape)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 escape <prompt>"
                exit 1
            fi
            _needle_escape_prompt_for_args "$2"
            ;;
        -h|--help)
            echo "Usage: $0 <command> [args]"
            echo ""
            echo "Commands:"
            echo "  render <template> <workspace> <prompt> <bead_id> <bead_title>"
            echo "      Render invoke template with variables"
            echo ""
            echo "  dispatch <agent_name> <workspace> <prompt> <bead_id> <bead_title> [timeout]"
            echo "      Dispatch agent and return exit_code|duration_ms|output_file"
            echo ""
            echo "  escape <prompt>"
            echo "      Escape prompt for args-style invocation"
            ;;
        *)
            echo "Unknown command: ${1:-}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
fi
