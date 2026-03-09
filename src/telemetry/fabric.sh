#!/usr/bin/env bash
# NEEDLE FABRIC Telemetry Forwarder
# Forwards parsed stream-json events to FABRIC dashboard in real-time
#
# This module intercepts stream-json output from agents and forwards
# structured events to a FABRIC endpoint for live visualization.

# Source dependencies if not already loaded
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

# Module version
_NEEDLE_FABRIC_VERSION="1.0.0"

# -----------------------------------------------------------------------------
# Configuration
# -----------------------------------------------------------------------------

# Get FABRIC endpoint from config or environment
# Priority: FABRIC_ENDPOINT env var > config file > disabled
# Usage: _needle_fabric_get_endpoint
# Returns: endpoint URL or empty string if disabled
_needle_fabric_get_endpoint() {
    # Check environment variable first
    if [[ -n "${FABRIC_ENDPOINT:-}" ]]; then
        echo "$FABRIC_ENDPOINT"
        return 0
    fi

    # Check if config loader is available
    if declare -f get_config &>/dev/null; then
        local enabled
        enabled=$(get_config "fabric.enabled" "false")

        if [[ "$enabled" == "true" ]]; then
            local endpoint
            endpoint=$(get_config "fabric.endpoint" "")
            if [[ -n "$endpoint" ]]; then
                echo "$endpoint"
                return 0
            fi
        fi
    fi

    # FABRIC not configured
    echo ""
    return 0
}

# Check if FABRIC forwarding is enabled
# Usage: _needle_fabric_is_enabled
# Returns: 0 if enabled, 1 if disabled
_needle_fabric_is_enabled() {
    local endpoint
    endpoint=$(_needle_fabric_get_endpoint)
    [[ -n "$endpoint" ]]
}

# Get FABRIC timeout for HTTP requests (in seconds)
# Usage: _needle_fabric_get_timeout
_needle_fabric_get_timeout() {
    if declare -f get_config &>/dev/null; then
        get_config "fabric.timeout" "2"
    else
        echo "2"
    fi
}

# Check if batching is enabled
# Usage: _needle_fabric_is_batching_enabled
_needle_fabric_is_batching_enabled() {
    if declare -f get_config_bool &>/dev/null; then
        local enabled
        enabled=$(get_config_bool "fabric.batching" "false")
        [[ "$enabled" == "true" ]]
    else
        return 1
    fi
}

# -----------------------------------------------------------------------------
# Event Forwarding
# -----------------------------------------------------------------------------

# Forward a single event to FABRIC endpoint
# Non-blocking: uses curl with timeout and runs in background
# Usage: _needle_fabric_forward_event <event_json>
# Returns: 0 always (failures are logged but don't block)
_needle_fabric_forward_event() {
    local event_json="$1"
    local endpoint
    endpoint=$(_needle_fabric_get_endpoint)

    if [[ -z "$endpoint" ]]; then
        return 0
    fi

    local timeout
    timeout=$(_needle_fabric_get_timeout)

    # Forward event in background with timeout
    # Stderr is suppressed to avoid noise in case of network issues
    (
        curl -X POST \
            -H "Content-Type: application/json" \
            -d "$event_json" \
            --max-time "$timeout" \
            --silent \
            --show-error \
            "$endpoint" &>/dev/null
    ) &

    # Don't wait for the background process
    return 0
}

# -----------------------------------------------------------------------------
# Stream Parser
# -----------------------------------------------------------------------------

# Parse and forward stream-json events from a file or stream
# Reads JSONL format and forwards each event to FABRIC
# Usage: _needle_fabric_parse_stream <input_file_or_pipe>
# Returns: 0 on success, 1 on error
_needle_fabric_parse_stream() {
    local input="$1"

    if ! _needle_fabric_is_enabled; then
        _needle_debug "FABRIC forwarding disabled, skipping stream parsing"
        return 0
    fi

    local endpoint
    endpoint=$(_needle_fabric_get_endpoint)
    _needle_debug "FABRIC forwarding enabled: $endpoint"

    # Read JSONL line by line
    while IFS= read -r line; do
        # Skip empty lines
        [[ -z "$line" ]] && continue

        # Try to parse as JSON (basic validation)
        # If jq is available, use it for validation
        if command -v jq &>/dev/null; then
            if echo "$line" | jq empty 2>/dev/null; then
                # Valid JSON, forward to FABRIC
                _needle_fabric_forward_event "$line"
            else
                _needle_debug "Skipping invalid JSON line: ${line:0:100}..."
            fi
        else
            # Without jq, just check if it looks like JSON
            if [[ "$line" =~ ^\{.*\}$ ]]; then
                _needle_fabric_forward_event "$line"
            fi
        fi
    done < "$input"

    return 0
}

# Parse and forward stream-json events from a live stream (follows file)
# Tails a file and forwards events as they appear
# Usage: _needle_fabric_follow_stream <output_file> <pid>
# Returns: when the process with <pid> exits
_needle_fabric_follow_stream() {
    local output_file="$1"
    local agent_pid="$2"

    if ! _needle_fabric_is_enabled; then
        return 0
    fi

    local endpoint
    endpoint=$(_needle_fabric_get_endpoint)
    _needle_debug "FABRIC live stream forwarding: $endpoint (following PID $agent_pid)"

    # Tail the file and process events as they arrive
    # Stop when the agent process exits
    tail -f "$output_file" 2>/dev/null | while IFS= read -r line; do
        # Check if agent is still running
        if ! kill -0 "$agent_pid" 2>/dev/null; then
            break
        fi

        # Skip empty lines
        [[ -z "$line" ]] && continue

        # Forward valid JSON events
        if command -v jq &>/dev/null; then
            if echo "$line" | jq empty 2>/dev/null; then
                _needle_fabric_forward_event "$line"
            fi
        else
            if [[ "$line" =~ ^\{.*\}$ ]]; then
                _needle_fabric_forward_event "$line"
            fi
        fi
    done

    return 0
}

# -----------------------------------------------------------------------------
# Batched Event Forwarding (Optional)
# -----------------------------------------------------------------------------

# Batch events and forward them together (reduces HTTP overhead)
# Usage: _needle_fabric_forward_batch <event_json_array>
_needle_fabric_forward_batch() {
    local events_json="$1"
    local endpoint
    endpoint=$(_needle_fabric_get_endpoint)

    if [[ -z "$endpoint" ]]; then
        return 0
    fi

    local timeout
    timeout=$(_needle_fabric_get_timeout)

    # Forward batch in background
    (
        curl -X POST \
            -H "Content-Type: application/json" \
            -d "$events_json" \
            --max-time "$timeout" \
            --silent \
            --show-error \
            "${endpoint}/batch" &>/dev/null
    ) &

    return 0
}

# -----------------------------------------------------------------------------
# Stream Tee Integration
# -----------------------------------------------------------------------------

# Create a named pipe for FABRIC event forwarding
# This allows us to tee the output to both the file and FABRIC parser
# Usage: _needle_fabric_create_pipe
# Returns: pipe path
_needle_fabric_create_pipe() {
    local pipe_path
    pipe_path=$(mktemp -u "${TMPDIR:-/tmp}/needle-fabric-pipe-XXXXXXXX")
    mkfifo "$pipe_path" 2>/dev/null || {
        _needle_warn "Failed to create FABRIC pipe: $pipe_path"
        echo ""
        return 1
    }
    echo "$pipe_path"
}

# Start FABRIC forwarder on a named pipe
# Usage: _needle_fabric_start_forwarder <pipe_path>
# Returns: PID of forwarder process
_needle_fabric_start_forwarder() {
    local pipe_path="$1"

    if ! _needle_fabric_is_enabled; then
        echo ""
        return 0
    fi

    # Start background process to read from pipe and forward events
    (
        while IFS= read -r line; do
            [[ -z "$line" ]] && continue

            # Forward if valid JSON
            if command -v jq &>/dev/null; then
                if echo "$line" | jq empty 2>/dev/null; then
                    _needle_fabric_forward_event "$line"
                fi
            else
                if [[ "$line" =~ ^\{.*\}$ ]]; then
                    _needle_fabric_forward_event "$line"
                fi
            fi
        done < "$pipe_path"
    ) &

    echo $!
}

# Stop FABRIC forwarder process
# Usage: _needle_fabric_stop_forwarder <forwarder_pid> <pipe_path>
_needle_fabric_stop_forwarder() {
    local forwarder_pid="$1"
    local pipe_path="$2"

    if [[ -n "$forwarder_pid" ]] && kill -0 "$forwarder_pid" 2>/dev/null; then
        kill "$forwarder_pid" 2>/dev/null
        wait "$forwarder_pid" 2>/dev/null
    fi

    if [[ -n "$pipe_path" ]] && [[ -p "$pipe_path" ]]; then
        rm -f "$pipe_path" 2>/dev/null
    fi
}

# -----------------------------------------------------------------------------
# Event Type Detection
# -----------------------------------------------------------------------------

# Detect event type from stream-json line
# Usage: _needle_fabric_detect_event_type <json_line>
# Returns: event type string (e.g., "tool_use", "thinking", "result")
_needle_fabric_detect_event_type() {
    local json_line="$1"

    if command -v jq &>/dev/null; then
        echo "$json_line" | jq -r '.type // "unknown"' 2>/dev/null
    else
        # Fallback: grep for type field
        echo "$json_line" | grep -oE '"type"[[:space:]]*:[[:space:]]*"[^"]*"' | \
            grep -oE '"[^"]*"$' | tr -d '"'
    fi
}

# Check if event should be forwarded to FABRIC
# Some events might be filtered based on configuration
# Usage: _needle_fabric_should_forward <event_type>
# Returns: 0 if should forward, 1 if should skip
_needle_fabric_should_forward() {
    local event_type="$1"

    # For now, forward all events
    # In the future, could add filtering based on config
    # e.g., fabric.event_filter: ["tool_use", "result", "thinking"]

    return 0
}

# -----------------------------------------------------------------------------
# Direct Execution Support (for testing)
# -----------------------------------------------------------------------------

# Allow running this module directly for testing
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-}" in
        endpoint)
            _needle_fabric_get_endpoint
            ;;
        enabled)
            if _needle_fabric_is_enabled; then
                echo "FABRIC forwarding: enabled"
                echo "Endpoint: $(_needle_fabric_get_endpoint)"
                exit 0
            else
                echo "FABRIC forwarding: disabled"
                exit 1
            fi
            ;;
        forward)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 forward <event_json>"
                exit 1
            fi
            _needle_fabric_forward_event "$2"
            ;;
        parse)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 parse <input_file>"
                exit 1
            fi
            _needle_fabric_parse_stream "$2"
            ;;
        test)
            # Test event forwarding
            local test_event='{"type":"test","ts":"2026-03-08T12:00:00.000Z","message":"FABRIC test event"}'
            echo "Testing FABRIC forwarding..."
            echo "Endpoint: $(_needle_fabric_get_endpoint)"
            if _needle_fabric_is_enabled; then
                echo "Sending test event: $test_event"
                _needle_fabric_forward_event "$test_event"
                echo "Event sent (check FABRIC dashboard)"
            else
                echo "FABRIC forwarding is disabled"
                echo "Set FABRIC_ENDPOINT environment variable or configure fabric.endpoint"
            fi
            ;;
        -h|--help)
            echo "Usage: $0 <command> [args]"
            echo ""
            echo "Commands:"
            echo "  endpoint                  Get FABRIC endpoint configuration"
            echo "  enabled                   Check if FABRIC forwarding is enabled"
            echo "  forward <event_json>      Forward a single event to FABRIC"
            echo "  parse <input_file>        Parse and forward events from JSONL file"
            echo "  test                      Send a test event to FABRIC"
            echo ""
            echo "Configuration:"
            echo "  FABRIC_ENDPOINT=http://localhost:3000/api/events"
            echo "  OR"
            echo "  fabric:"
            echo "    enabled: true"
            echo "    endpoint: http://localhost:3000/api/events"
            ;;
        *)
            echo "Unknown command: ${1:-}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
fi
