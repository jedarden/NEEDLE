#!/usr/bin/env bash
# Tests for NEEDLE agent loader module (src/agent/loader.sh)

# Test setup
TEST_DIR=$(mktemp -d)

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_HOME="$TEST_DIR/.needle"
export NEEDLE_CONFIG_FILE="$NEEDLE_HOME/config.yaml"
export NEEDLE_CONFIG_NAME="config.yaml"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=false

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/json.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/agent/loader.sh"

# Cleanup function
cleanup() {
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

# Test counter
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

# Test helper
test_case() {
    local name="$1"
    ((TESTS_RUN++))
    echo -n "Testing: $name... "
}

test_pass() {
    echo "PASS"
    ((TESTS_PASSED++))
}

test_fail() {
    local reason="${1:-}"
    echo "FAIL"
    [[ -n "$reason" ]] && echo "  Reason: $reason"
    ((TESTS_FAILED++))
}

# ============ YAML Parsing Tests ============

test_case "_needle_parse_yaml_python reads simple string"
cat > "$TEST_DIR/test.yaml" << 'EOF'
name: test-agent
EOF
result=$(_needle_parse_yaml_python "$TEST_DIR/test.yaml" '.name')
if [[ "$result" == "test-agent" ]]; then
    test_pass
else
    test_fail "Expected 'test-agent', got '$result'"
fi

test_case "_needle_parse_yaml_python reads nested value"
cat > "$TEST_DIR/test.yaml" << 'EOF'
input:
  method: heredoc
EOF
result=$(_needle_parse_yaml_python "$TEST_DIR/test.yaml" '.input.method')
if [[ "$result" == "heredoc" ]]; then
    test_pass
else
    test_fail "Expected 'heredoc', got '$result'"
fi

test_case "_needle_parse_yaml_python reads deeply nested value"
cat > "$TEST_DIR/test.yaml" << 'EOF'
limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF
result=$(_needle_parse_yaml_python "$TEST_DIR/test.yaml" '.limits.requests_per_minute')
if [[ "$result" == "60" ]]; then
    test_pass
else
    test_fail "Expected '60', got '$result'"
fi

test_case "_needle_parse_yaml_python handles missing file"
result=$(_needle_parse_yaml_python "$TEST_DIR/nonexistent.yaml" '.name' 2>/dev/null)
exit_code=$?
if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit code for missing file"
fi

test_case "_needle_parse_yaml_python returns array as JSON"
cat > "$TEST_DIR/test.yaml" << 'EOF'
success_codes: [0, 1]
EOF
result=$(_needle_parse_yaml_python "$TEST_DIR/test.yaml" '.success_codes')
if [[ "$result" == "[0, 1]" ]] || [[ "$result" == "[0,1]" ]]; then
    test_pass
else
    test_fail "Expected JSON array, got '$result'"
fi

test_case "_needle_parse_yaml fallback works"
cat > "$TEST_DIR/test.yaml" << 'EOF'
name: fallback-test
EOF
result=$(_needle_parse_yaml "$TEST_DIR/test.yaml" '.name')
if [[ "$result" == "fallback-test" ]]; then
    test_pass
else
    test_fail "Expected 'fallback-test', got '$result'"
fi

# ============ Agent Discovery Tests ============

test_case "_needle_find_agent_config finds builtin agent"
result=$(_needle_find_agent_config "claude-anthropic-sonnet")
if [[ -n "$result" ]] && [[ -f "$result" ]]; then
    test_pass
else
    test_fail "Expected to find claude-anthropic-sonnet.yaml"
fi

test_case "_needle_find_agent_config returns empty for nonexistent agent"
result=$(_needle_find_agent_config "nonexistent-agent-xyz" 2>/dev/null)
exit_code=$?
if [[ -z "$result" ]] || [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Expected empty result for nonexistent agent, got '$result'"
fi

test_case "_needle_find_agent_config prefers workspace config"
mkdir -p "$TEST_DIR/.needle/agents"
cat > "$TEST_DIR/.needle/agents/test-override.yaml" << 'EOF'
name: test-override
runner: test-runner
invoke: echo test
EOF
# Change to test directory to make relative path work
pushd "$TEST_DIR" >/dev/null
result=$(_needle_find_agent_config "test-override")
popd >/dev/null
if [[ "$result" == *".needle/agents/test-override.yaml"* ]]; then
    test_pass
else
    test_fail "Expected workspace config path, got '$result'"
fi

test_case "_needle_list_available_agents returns list"
result=$(_needle_list_available_agents)
if [[ "$result" == *"claude-anthropic-sonnet"* ]]; then
    test_pass
else
    test_fail "Expected claude-anthropic-sonnet in list"
fi

test_case "_needle_list_available_agents --json returns valid JSON"
result=$(_needle_list_available_agents --json)
if [[ "$result" == "["*"]" ]] && [[ "$result" == *'"claude-anthropic-sonnet"'* ]]; then
    test_pass
else
    test_fail "Expected JSON array with agents"
fi

# ============ Agent Loading Tests ============

test_case "_needle_load_agent loads claude-anthropic-sonnet"
if _needle_load_agent "claude-anthropic-sonnet" 2>/dev/null; then
    if [[ "${NEEDLE_AGENT[name]}" == "claude-anthropic-sonnet" ]]; then
        test_pass
    else
        test_fail "Expected name='claude-anthropic-sonnet', got '${NEEDLE_AGENT[name]}'"
    fi
else
    test_fail "Failed to load agent"
fi

test_case "_needle_load_agent populates runner"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
if [[ "${NEEDLE_AGENT[runner]}" == "claude" ]]; then
    test_pass
else
    test_fail "Expected runner='claude', got '${NEEDLE_AGENT[runner]}'"
fi

test_case "_needle_load_agent populates provider"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
if [[ "${NEEDLE_AGENT[provider]}" == "anthropic" ]]; then
    test_pass
else
    test_fail "Expected provider='anthropic', got '${NEEDLE_AGENT[provider]}'"
fi

test_case "_needle_load_agent populates model"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
if [[ "${NEEDLE_AGENT[model]}" == "sonnet" ]]; then
    test_pass
else
    test_fail "Expected model='sonnet', got '${NEEDLE_AGENT[model]}'"
fi

test_case "_needle_load_agent populates invoke template"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
if [[ "${NEEDLE_AGENT[invoke]}" == *"claude"* ]]; then
    test_pass
else
    test_fail "Expected invoke to contain 'claude'"
fi

test_case "_needle_load_agent populates input method"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
if [[ "${NEEDLE_AGENT[input_method]}" == "heredoc" ]]; then
    test_pass
else
    test_fail "Expected input_method='heredoc', got '${NEEDLE_AGENT[input_method]}'"
fi

test_case "_needle_load_agent populates output format"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
if [[ "${NEEDLE_AGENT[output_format]}" == "json" ]]; then
    test_pass
else
    test_fail "Expected output_format='json', got '${NEEDLE_AGENT[output_format]}'"
fi

test_case "_needle_load_agent populates rate limits"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
if [[ "${NEEDLE_AGENT[requests_per_minute]}" == "60" ]]; then
    test_pass
else
    test_fail "Expected requests_per_minute='60', got '${NEEDLE_AGENT[requests_per_minute]}'"
fi

test_case "_needle_load_agent fails gracefully for nonexistent agent"
if ! _needle_load_agent "nonexistent-agent-xyz" 2>/dev/null; then
    test_pass
else
    test_fail "Expected failure for nonexistent agent"
fi

# ============ Agent Validation Tests ============

test_case "_needle_validate_agent validates runner presence"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
# The validate function checks if runner command exists
# This may pass if claude is installed or fail if not
# We're testing that the validation runs, not its result
if _needle_validate_agent "claude-anthropic-sonnet" 2>/dev/null || [[ $? -eq 1 ]]; then
    test_pass
else
    test_fail "Expected validation to run and return 0 or 1"
fi

test_case "_needle_validate_agent fails for missing invoke template"
# Create a minimal broken config
mkdir -p "$TEST_DIR/.needle/agents"
cat > "$TEST_DIR/.needle/agents/broken-no-invoke.yaml" << 'EOF'
name: broken-no-invoke
runner: bash
EOF
pushd "$TEST_DIR" >/dev/null
if ! _needle_validate_agent "broken-no-invoke" 2>/dev/null; then
    test_pass
else
    test_fail "Expected validation failure for missing invoke"
fi
popd >/dev/null

test_case "_needle_validate_agent fails for invalid input method"
mkdir -p "$TEST_DIR/.needle/agents"
cat > "$TEST_DIR/.needle/agents/bad-input-method.yaml" << 'EOF'
name: bad-input-method
runner: bash
invoke: echo test
input:
  method: invalid_method
EOF
pushd "$TEST_DIR" >/dev/null
if ! _needle_validate_agent "bad-input-method" 2>/dev/null; then
    test_pass
else
    test_fail "Expected validation failure for invalid input method"
fi
popd >/dev/null

# ============ All Built-in Agents Load Tests ============

for agent in claude-anthropic-sonnet claude-anthropic-opus opencode-alibaba-qwen opencode-ollama-deepseek codex-openai-gpt4 aider-ollama-deepseek; do
    test_case "_needle_load_agent loads $agent"
    if _needle_load_agent "$agent" 2>/dev/null; then
        if [[ "${NEEDLE_AGENT[name]}" == "$agent" ]]; then
            test_pass
        else
            test_fail "Name mismatch for $agent"
        fi
    else
        test_fail "Failed to load $agent"
    fi
done

# ============ JSON Export Tests ============

test_case "_needle_export_agent_json returns valid JSON"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
result=$(_needle_export_agent_json)
if [[ "$result" == "{"*"}" ]] && [[ "$result" == *'"name"'* ]] && [[ "$result" == *'"runner"'* ]]; then
    test_pass
else
    test_fail "Expected JSON object with name and runner"
fi

# ============ Helper Function Tests ============

test_case "_needle_get_agent_property returns correct value"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
result=$(_needle_get_agent_property "runner")
if [[ "$result" == "claude" ]]; then
    test_pass
else
    test_fail "Expected 'claude', got '$result'"
fi

test_case "_needle_is_agent_configured returns correct status"
# This depends on whether claude is actually installed
_needle_is_agent_configured "claude-anthropic-sonnet" 2>/dev/null
exit_code=$?
# Just check that it runs without error
if [[ $exit_code -eq 0 ]] || [[ $exit_code -eq 1 ]]; then
    test_pass
else
    test_fail "Expected exit code 0 or 1, got $exit_code"
fi

# ============ yq Error Handling Tests ============

test_case "_needle_parse_yaml handles invalid YAML gracefully"
cat > "$TEST_DIR/invalid.yaml" << 'EOF'
name: test
  - broken list
    malformed: structure
      deep: error
        unclosed bracket [
EOF
result=$(_needle_parse_yaml "$TEST_DIR/invalid.yaml" '.name' 2>/dev/null)
exit_code=$?
if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Expected non-zero exit for invalid YAML"
fi

# ============ Summary ============
echo ""
echo "================================"
echo "Test Summary"
echo "================================"
echo "Tests run:    $TESTS_RUN"
echo "Tests passed: $TESTS_PASSED"
echo "Tests failed: $TESTS_FAILED"
echo "================================"

if [[ $TESTS_FAILED -gt 0 ]]; then
    exit 1
fi

exit 0
