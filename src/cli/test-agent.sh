#!/usr/bin/env bash
# NEEDLE CLI Test-Agent Subcommand
# Test agent adapter configurations

# Help function for test-agent command
_needle_test_agent_help() {
    _needle_print "Test an agent adapter configuration

Validates an agent adapter by running a simple test prompt and
checking the output format.

USAGE:
    needle test-agent <AGENT> [OPTIONS]

ARGUMENTS:
    <AGENT>     Agent adapter name (e.g., claude-anthropic-sonnet)

OPTIONS:
    -p, --prompt <TEXT>      Custom test prompt [default: \"echo hello\"]
    -t, --timeout <SECS>     Timeout for test [default: 60]
    -v, --verbose            Show full agent output

    -h, --help               Print help information

TESTS PERFORMED:
    1. Agent CLI exists in PATH
    2. Invoke template renders correctly
    3. Agent executes and exits with code 0
    4. Output is parseable

EXAMPLES:
    # Test an agent
    needle test-agent claude-anthropic-sonnet

    # Test with custom prompt
    needle test-agent opencode-alibaba-qwen --prompt=\"print hello world\"

    # Verbose output for debugging
    needle test-agent claude-anthropic-sonnet --verbose

EXIT CODES:
    0    Agent working correctly
    1    Agent CLI not found
    2    Execution failed
    3    Output parsing failed
"
}

# Main test-agent function
_needle_test_agent() {
    local agent=""
    local prompt="echo 'Hello from NEEDLE test'"
    local timeout=60
    local verbose=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -p|--prompt)
                if [[ -z "${2:-}" ]]; then
                    _needle_error "Option $1 requires an argument"
                    exit $NEEDLE_EXIT_USAGE
                fi
                prompt="$2"
                shift 2
                ;;
            -t|--timeout)
                if [[ -z "${2:-}" ]]; then
                    _needle_error "Option $1 requires an argument"
                    exit $NEEDLE_EXIT_USAGE
                fi
                if ! [[ "$2" =~ ^[0-9]+$ ]]; then
                    _needle_error "Timeout must be a positive integer"
                    exit $NEEDLE_EXIT_USAGE
                fi
                timeout="$2"
                shift 2
                ;;
            -v|--verbose)
                verbose=true
                shift
                ;;
            -h|--help)
                _needle_test_agent_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            -*)
                _needle_error "Unknown option: $1"
                _needle_print ""
                _needle_test_agent_help
                exit $NEEDLE_EXIT_USAGE
                ;;
            *)
                # Positional argument - agent name
                if [[ -z "$agent" ]]; then
                    agent="$1"
                else
                    _needle_error "Unexpected argument: $1"
                    _needle_print ""
                    _needle_test_agent_help
                    exit $NEEDLE_EXIT_USAGE
                fi
                shift
                ;;
        esac
    done

    # Validate agent name is provided
    if [[ -z "$agent" ]]; then
        _needle_error "Agent name required"
        _needle_print ""
        _needle_test_agent_help
        exit $NEEDLE_EXIT_USAGE
    fi

    _needle_info "Testing agent: $agent"
    _needle_print ""

    local test_passed=0
    local test_failed=0

    # Test 1: Agent config exists
    echo -n "1. Agent config exists... "
    if ! _needle_load_agent "$agent" 2>/dev/null; then
        echo "FAIL"
        _needle_error "Agent configuration not found: $agent"
        _needle_info "Searched in: ${NEEDLE_AGENT_PATHS[*]}"
        exit $NEEDLE_EXIT_CONFIG
    fi
    echo "OK"
    test_passed=$((test_passed + 1))

    # Test 2: Runner exists in PATH
    echo -n "2. Runner '${NEEDLE_AGENT[runner]}' in PATH... "
    if ! command -v "${NEEDLE_AGENT[runner]}" &>/dev/null; then
        echo "FAIL"
        _needle_error "Runner not found in PATH: ${NEEDLE_AGENT[runner]}"
        _needle_info "Install the runner or verify it's in your PATH"
        exit $NEEDLE_EXIT_CONFIG
    fi
    echo "OK"
    test_passed=$((test_passed + 1))

    # Test 3: Invoke template renders
    echo -n "3. Invoke template renders... "
    local rendered
    rendered=$(_needle_render_invoke \
        "${NEEDLE_AGENT[invoke]}" \
        "/tmp" \
        "$prompt" \
        "test-bead" \
        "Test bead"
    )

    if [[ -z "$rendered" ]]; then
        echo "FAIL"
        _needle_error "Failed to render invoke template"
        exit $NEEDLE_EXIT_CONFIG
    fi
    echo "OK"
    test_passed=$((test_passed + 1))

    # Test 4: Agent executes
    echo -n "4. Agent executes successfully... "
    local output_file
    output_file=$(mktemp "${TMPDIR:-/tmp}/needle-test-agent-XXXXXXXX.log")
    local exit_code

    # Determine input method and dispatch accordingly
    local input_method="${NEEDLE_AGENT[input_method]:-heredoc}"

    case "$input_method" in
        heredoc)
            if timeout "$timeout" bash -c "$rendered" > "$output_file" 2>&1; then
                exit_code=0
            else
                exit_code=$?
            fi
            ;;
        stdin)
            if echo "$prompt" | timeout "$timeout" bash -c "${NEEDLE_AGENT[invoke]}" > "$output_file" 2>&1; then
                exit_code=0
            else
                exit_code=$?
            fi
            ;;
        file)
            local file_path="${TMPDIR:-/tmp}/needle-test-prompt-$$.txt"
            echo "$prompt" > "$file_path"
            local resolved_cmd="${NEEDLE_AGENT[invoke]//\$\{PROMPT_FILE\}/$file_path}"
            if timeout "$timeout" bash -c "$resolved_cmd" > "$output_file" 2>&1; then
                exit_code=0
            else
                exit_code=$?
            fi
            rm -f "$file_path" 2>/dev/null
            ;;
        args)
            # For args, use escaped prompt
            local escaped_prompt
            escaped_prompt=$(_needle_escape_prompt_for_args "$prompt")
            local rendered_args="${NEEDLE_AGENT[invoke]}"
            rendered_args="${rendered_args//\$\{WORKSPACE\}/\/tmp}"
            rendered_args="${rendered_args//\$\{BEAD_ID\}/test-bead}"
            rendered_args="${rendered_args//\$\{BEAD_TITLE\}/Test bead}"
            rendered_args="${rendered_args//\$\{PROMPT\}/$escaped_prompt}"

            if timeout "$timeout" bash -c "$rendered_args" > "$output_file" 2>&1; then
                exit_code=0
            else
                exit_code=$?
            fi
            ;;
        *)
            echo "FAIL"
            _needle_error "Unknown input method: $input_method"
            rm -f "$output_file"
            exit $NEEDLE_EXIT_CONFIG
            ;;
    esac

    # Check exit code
    if [[ $exit_code -eq 0 ]]; then
        echo "OK"
        test_passed=$((test_passed + 1))
    elif [[ $exit_code -eq 124 ]]; then
        echo "FAIL (timeout after ${timeout}s)"
        test_failed=$((test_failed + 1))
    else
        echo "FAIL (exit code: $exit_code)"
        test_failed=$((test_failed + 1))
    fi

    # Show verbose output if requested
    if $verbose; then
        _needle_print ""
        _needle_section "--- Agent Output ---"
        cat "$output_file"
        _needle_section "--- End Output ---"
    fi

    # Cleanup
    rm -f "$output_file"

    # Summary
    _needle_print ""
    _needle_verbose "Tests passed: $test_passed"
    [[ $test_failed -gt 0 ]] && _needle_verbose "Tests failed: $test_failed"

    if [[ $test_failed -eq 0 ]]; then
        _needle_success "Agent '$agent' is working correctly"
        exit $NEEDLE_EXIT_SUCCESS
    else
        _needle_error "Agent '$agent' test failed"
        exit $NEEDLE_EXIT_ERROR
    fi
}
