#!/usr/bin/env bash
# NEEDLE Bead Verification Module
# Post-execution verification of bead completion
#
# Implementation: nd-lroc (Part 1: nd-lvx2)
#
# This module provides:
# - Runs verification_cmd after agent execution
# - Retry logic for flaky verification commands
# - Formats re-dispatch context on verification failure
# - Labels beads for human review when needed
#
# Usage:
#   _needle_verify_bead <bead_id> <workspace> [--max-retries <n>]
#
# Return values:
#   0 - Verification passed (bead is done)
#   1 - Verification failed (needs re-dispatch or release)
#   2 - No verification_cmd defined (skip verification)

# Source dependencies (if not already loaded)
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

if [[ -z "${_NEEDLE_CONSTANTS_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/constants.sh"
fi

# Source telemetry for events
if [[ -z "${_NEEDLE_TELEMETRY_EVENTS_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../telemetry/events.sh"
fi

# Source diagnostic module for logging
if [[ -z "${_NEEDLE_DIAGNOSTIC_LOADED:-}" ]]; then
    NEEDLE_SRC="${NEEDLE_SRC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
    source "$NEEDLE_SRC/lib/diagnostic.sh"
fi

# ============================================================================
# Configuration
# ============================================================================

# Default retry configuration
NEEDLE_VERIFY_MAX_RETRIES="${NEEDLE_VERIFY_MAX_RETRIES:-3}"
NEEDLE_VERIFY_RETRY_DELAY="${NEEDLE_VERIFY_RETRY_DELAY:-2}"

# ============================================================================
# Verification Command Extraction
# ============================================================================

# Extract verification_cmd from bead JSON
# The verification_cmd can be stored in:
#   - metadata.verification_cmd (preferred)
#   - A label starting with "verification_cmd:" (fallback)
#
# Usage: _needle_get_verification_cmd <bead_id> [workspace]
# Returns: verification_cmd string or empty if not defined
# Exit codes:
#   0 - Command found (output to stdout)
#   1 - No verification_cmd defined
_needle_get_verification_cmd() {
    local bead_id="$1"
    local workspace="${2:-${NEEDLE_WORKSPACE:-$(pwd)}}"

    if [[ -z "$bead_id" ]]; then
        return 1
    fi

    # Get bead JSON - must run in workspace context
    local bead_json
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        bead_json=$(cd "$workspace" && br show "$bead_id" --json 2>/dev/null)
    else
        bead_json=$(br show "$bead_id" --json 2>/dev/null)
    fi

    if [[ -z "$bead_json" ]] || [[ "$bead_json" == "null" ]]; then
        _needle_debug "Could not retrieve bead JSON for verification_cmd extraction"
        return 1
    fi

    # Handle array response
    if echo "$bead_json" | jq -e 'type == "array"' &>/dev/null; then
        bead_json=$(echo "$bead_json" | jq -c '.[0]')
    fi

    # Try metadata.verification_cmd first (preferred location)
    local verification_cmd
    verification_cmd=$(echo "$bead_json" | jq -r '.metadata.verification_cmd // empty' 2>/dev/null)

    if [[ -n "$verification_cmd" ]]; then
        echo "$verification_cmd"
        return 0
    fi

    # Fallback: check labels for "verification_cmd:<command>" pattern
    local labels
    labels=$(echo "$bead_json" | jq -r '.labels[]? // empty' 2>/dev/null)

    while IFS= read -r label; do
        if [[ "$label" =~ ^verification_cmd:(.+)$ ]]; then
            echo "${BASH_REMATCH[1]}"
            return 0
        fi
    done <<< "$labels"

    # No verification_cmd found
    return 1
}

# ============================================================================
# Verification Execution
# ============================================================================

# Run verification command with retry logic
#
# Usage: _needle_run_verification <bead_id> <workspace> [--max-retries <n>]
# Returns: JSON object with verification results
# Exit codes:
#   0 - Verification passed
#   1 - Verification failed after all retries
#   2 - No verification_cmd defined
#
# Output JSON structure:
#   {
#     "passed": true|false,
#     "attempts": <n>,
#     "command": "<verification_cmd>",
#     "output": "<last_output>",
#     "exit_code": <last_exit_code>,
#     "flaky": true|false
#   }
_needle_run_verification() {
    local bead_id="$1"
    local workspace="${2:-${NEEDLE_WORKSPACE:-$(pwd)}}"
    local max_retries="${NEEDLE_VERIFY_MAX_RETRIES:-3}"
    local retry_delay="${NEEDLE_VERIFY_RETRY_DELAY:-2}"

    # Parse optional arguments
    shift 2 2>/dev/null || true
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --max-retries)
                max_retries="$2"
                shift 2
                ;;
            --retry-delay)
                retry_delay="$2"
                shift 2
                ;;
            *)
                shift
                ;;
        esac
    done

    # Get verification command
    local verification_cmd
    verification_cmd=$(_needle_get_verification_cmd "$bead_id" "$workspace")

    if [[ -z "$verification_cmd" ]]; then
        _needle_debug "No verification_cmd defined for bead $bead_id"
        echo '{"passed":true,"attempts":0,"command":null,"output":null,"exit_code":0,"flaky":false,"skipped":true}'
        return 2
    fi

    _needle_debug "Running verification for bead $bead_id: $verification_cmd"

    # Track attempts and results
    local attempt=1
    local passed=false
    local last_exit_code=0
    local last_output=""
    local success_on_first=false

    while [[ $attempt -le $max_retries ]]; do
        _needle_debug "Verification attempt $attempt/$max_retries"

        # Run the verification command
        # Commands run in workspace context
        local cmd_output
        local cmd_exit

        if [[ -n "$workspace" && -d "$workspace" ]]; then
            cmd_output=$(cd "$workspace" && eval "$verification_cmd" 2>&1)
            cmd_exit=$?
        else
            cmd_output=$(eval "$verification_cmd" 2>&1)
            cmd_exit=$?
        fi

        last_output="$cmd_output"
        last_exit_code=$cmd_exit

        _needle_debug "Verification attempt $attempt: exit_code=$cmd_exit"

        if [[ $cmd_exit -eq 0 ]]; then
            passed=true
            if [[ $attempt -eq 1 ]]; then
                success_on_first=true
            fi

            # Emit telemetry
            _needle_telemetry_emit "bead.verification_passed" "info" \
                "bead_id=$bead_id" \
                "attempts=$attempt" \
                "command=${verification_cmd:0:100}"

            break
        fi

        # Failed - emit retry telemetry
        _needle_telemetry_emit "bead.verification_retry" "warn" \
            "bead_id=$bead_id" \
            "attempt=$attempt" \
            "exit_code=$cmd_exit" \
            "output_preview=${cmd_output:0:200}"

        # Wait before retry (unless this is the last attempt)
        if [[ $attempt -lt $max_retries ]]; then
            _needle_debug "Waiting ${retry_delay}s before retry..."
            sleep "$retry_delay"
        fi

        ((attempt++))
    done

    # Determine if flaky (succeeded but not on first try)
    local is_flaky=false
    if $passed && ! $success_on_first; then
        is_flaky=true
    fi

    # Build result JSON
    local result_json
    result_json=$(jq -n \
        --argjson passed "$passed" \
        --argjson attempts "$attempt" \
        --arg command "$verification_cmd" \
        --arg output "$last_output" \
        --argjson exit_code "$last_exit_code" \
        --argjson flaky "$is_flaky" \
        --argjson skipped false \
        '{
            passed: $passed,
            attempts: $attempts,
            command: $command,
            output: $output,
            exit_code: $exit_code,
            flaky: $flaky,
            skipped: $skipped
        }')

    echo "$result_json"

    if $passed; then
        return 0
    else
        return 1
    fi
}

# ============================================================================
# Verification Failure Handling
# ============================================================================

# Format re-dispatch context after verification failure
# This context will be appended to the prompt when re-dispatching
#
# Usage: _needle_format_verification_failure_context <verification_result_json>
# Returns: Formatted context string for re-dispatch
_needle_format_verification_failure_context() {
    local result_json="$1"

    if [[ -z "$result_json" ]]; then
        echo "## Verification Failed\nNo details available."
        return 0
    fi

    local passed attempts command output exit_code flaky
    passed=$(echo "$result_json" | jq -r '.passed')
    attempts=$(echo "$result_json" | jq -r '.attempts')
    command=$(echo "$result_json" | jq -r '.command')
    output=$(echo "$result_json" | jq -r '.output // "No output"')
    exit_code=$(echo "$result_json" | jq -r '.exit_code')
    flaky=$(echo "$result_json" | jq -r '.flaky')

    cat <<EOF
## Verification Failed

The verification command did not pass. Please review and correct your implementation.

**Verification Command:**
\`\`\`bash
$command
\`\`\`

**Result:** Failed after $attempts attempt(s) (exit code: $exit_code)

**Output:**
\`\`\`
$output
\`\`\`

$(if [[ "$flaky" == "true" ]]; then
    echo "⚠️ Note: This verification command has shown flaky behavior in the past."
fi)

Please ensure your implementation passes the verification command before marking this task complete.
EOF
}

# Add verification-flaky label to bead
# Used when verification passes after retries (indicates flaky test/cmd)
#
# Usage: _needle_label_verification_flaky <bead_id> [workspace]
# Returns: 0 on success, 1 on failure
_needle_label_verification_flaky() {
    local bead_id="$1"
    local workspace="${2:-${NEEDLE_WORKSPACE:-$(pwd)}}"

    if [[ -z "$bead_id" ]]; then
        return 1
    fi

    _needle_debug "Adding verification-flaky label to bead $bead_id"

    # Use br update to add the label
    local update_result
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        update_result=$(cd "$workspace" && br update "$bead_id" --label "verification-flaky" 2>&1)
    else
        update_result=$(br update "$bead_id" --label "verification-flaky" 2>&1)
    fi
    local update_exit=$?

    if [[ $update_exit -eq 0 ]]; then
        _needle_info "Labeled bead $bead_id as verification-flaky for human review"
        return 0
    else
        _needle_warn "Failed to add verification-flaky label: $update_result"
        return 1
    fi
}

# ============================================================================
# Main Verification Entry Point
# ============================================================================

# Main verification function - runs verification and handles flaky labeling
#
# Usage: _needle_verify_bead <bead_id> <workspace> [options]
# Options:
#   --max-retries <n>    Maximum retry attempts (default: 3)
#   --retry-delay <n>    Delay between retries in seconds (default: 2)
#
# Returns: JSON result object to stdout
# Exit codes:
#   0 - Verification passed
#   1 - Verification failed
#   2 - No verification_cmd defined (skip)
#
# Side effects:
#   - Adds verification-flaky label if command passed after retries
_needle_verify_bead() {
    local bead_id="$1"
    local workspace="${2:-${NEEDLE_WORKSPACE:-$(pwd)}}"
    shift 2 2>/dev/null || true

    # Run verification
    local result_json
    result_json=$(_needle_run_verification "$bead_id" "$workspace" "$@")
    local verify_exit=$?

    # Check if verification was skipped
    if [[ $verify_exit -eq 2 ]]; then
        echo "$result_json"
        return 2
    fi

    # If passed but flaky, add label for human review
    local is_flaky
    is_flaky=$(echo "$result_json" | jq -r '.flaky')
    if [[ "$is_flaky" == "true" ]]; then
        _needle_label_verification_flaky "$bead_id" "$workspace"
    fi

    # Emit telemetry for result
    local passed
    passed=$(echo "$result_json" | jq -r '.passed')
    if [[ "$passed" == "true" ]]; then
        _needle_event_bead_verified "$bead_id" \
            "workspace=$workspace" \
            "attempts=$(echo "$result_json" | jq -r '.attempts')" \
            "flaky=$is_flaky"
    else
        _needle_telemetry_emit "bead.verification_failed" "error" \
            "bead_id=$bead_id" \
            "workspace=$workspace" \
            "attempts=$(echo "$result_json" | jq -r '.attempts')"
    fi

    echo "$result_json"
    return $verify_exit
}

# ============================================================================
# Direct Execution Support (for testing)
# ============================================================================

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-}" in
        get-cmd)
            if [[ $# -lt 2 ]]; then
                echo "Usage: $0 get-cmd <bead_id> [workspace]"
                exit 1
            fi
            _needle_get_verification_cmd "$2" "${3:-}"
            exit $?
            ;;
        run)
            if [[ $# -lt 3 ]]; then
                echo "Usage: $0 run <bead_id> <workspace> [--max-retries <n>]"
                exit 1
            fi
            shift
            _needle_run_verification "$@"
            exit $?
            ;;
        verify)
            if [[ $# -lt 3 ]]; then
                echo "Usage: $0 verify <bead_id> <workspace> [--max-retries <n>]"
                exit 1
            fi
            shift
            _needle_verify_bead "$@"
            exit $?
            ;;
        format-failure)
            if [[ $# -lt 2 ]]; then
                echo "Usage: $0 format-failure '<result_json>'"
                exit 1
            fi
            _needle_format_verification_failure_context "$2"
            exit 0
            ;;
        label-flaky)
            if [[ $# -lt 2 ]]; then
                echo "Usage: $0 label-flaky <bead_id> [workspace]"
                exit 1
            fi
            _needle_label_verification_flaky "$2" "${3:-}"
            exit $?
            ;;
        -h|--help)
            echo "Usage: $0 <command> [options]"
            echo ""
            echo "Commands:"
            echo "  get-cmd <bead_id> [workspace]     Extract verification_cmd from bead"
            echo "  run <bead_id> <workspace> [opts]  Run verification with retries"
            echo "  verify <bead_id> <workspace>      Main verification entry point"
            echo "  format-failure '<json>'           Format failure context for re-dispatch"
            echo "  label-flaky <bead_id> [ws]        Add verification-flaky label"
            echo ""
            echo "Options for run/verify:"
            echo "  --max-retries <n>  Maximum retry attempts (default: 3)"
            echo "  --retry-delay <n>  Delay between retries in seconds (default: 2)"
            echo ""
            echo "Environment Variables:"
            echo "  NEEDLE_VERIFY_MAX_RETRIES   Default max retries (default: 3)"
            echo "  NEEDLE_VERIFY_RETRY_DELAY   Default retry delay (default: 2)"
            echo ""
            echo "Exit codes:"
            echo "  0 - Verification passed"
            echo "  1 - Verification failed"
            echo "  2 - No verification_cmd defined"
            ;;
        *)
            echo "Unknown command: ${1:-}" >&2
            echo "Use --help for usage information" >&2
            exit 1
            ;;
    esac
fi
