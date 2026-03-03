#!/usr/bin/env bash
# Tests for NEEDLE Agent Adapter System
# Comprehensive test suite covering adapter loading, invocation, and output parsing
#
# This test suite covers:
# - Adapter Loading (integration with agent/loader.sh)
# - Agent Invocation (integration with agent/dispatch.sh)
# - Prompt Escaping (integration with agent/escape.sh)
# - Token Extraction (integration with telemetry/tokens.sh)
# - Built-in Adapters (config/agents/*.yaml)
# - Custom Adapter Loading
# - Error Handling

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
source "$PROJECT_DIR/src/agent/escape.sh"
source "$PROJECT_DIR/src/agent/loader.sh"
source "$PROJECT_DIR/src/agent/dispatch.sh"
source "$PROJECT_DIR/src/telemetry/tokens.sh"
source "$PROJECT_DIR/src/telemetry/effort.sh"

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

# =============================================================================
# SECTION 1: Built-in Adapter Loading Tests
# =============================================================================

echo ""
echo "=== Built-in Adapter Loading Tests ==="

test_case "Loads claude-anthropic-sonnet adapter"
if _needle_load_agent "claude-anthropic-sonnet" 2>/dev/null; then
    if [[ "${NEEDLE_AGENT[runner]}" == "claude" ]] && \
       [[ "${NEEDLE_AGENT[provider]}" == "anthropic" ]] && \
       [[ "${NEEDLE_AGENT[input_method]}" == "heredoc" ]] && \
       [[ "${NEEDLE_AGENT[output_format]}" == "json" ]]; then
        test_pass
    else
        test_fail "Adapter fields mismatch"
    fi
else
    test_fail "Failed to load adapter"
fi

test_case "Loads claude-anthropic-opus adapter"
if _needle_load_agent "claude-anthropic-opus" 2>/dev/null; then
    if [[ "${NEEDLE_AGENT[runner]}" == "claude" ]] && \
       [[ "${NEEDLE_AGENT[model]}" == "opus" ]]; then
        test_pass
    else
        test_fail "Adapter fields mismatch"
    fi
else
    test_fail "Failed to load adapter"
fi

test_case "Loads opencode-alibaba-qwen adapter"
if _needle_load_agent "opencode-alibaba-qwen" 2>/dev/null; then
    if [[ "${NEEDLE_AGENT[runner]}" == "opencode" ]] && \
       [[ "${NEEDLE_AGENT[provider]}" == "alibaba" ]]; then
        test_pass
    else
        test_fail "Adapter fields mismatch"
    fi
else
    test_fail "Failed to load adapter"
fi

test_case "Loads opencode-ollama-deepseek adapter"
if _needle_load_agent "opencode-ollama-deepseek" 2>/dev/null; then
    if [[ "${NEEDLE_AGENT[runner]}" == "opencode" ]] && \
       [[ "${NEEDLE_AGENT[provider]}" == "ollama" ]]; then
        test_pass
    else
        test_fail "Adapter fields mismatch"
    fi
else
    test_fail "Failed to load adapter"
fi

test_case "Loads codex-openai-gpt4 adapter"
if _needle_load_agent "codex-openai-gpt4" 2>/dev/null; then
    if [[ "${NEEDLE_AGENT[runner]}" == "codex" ]] && \
       [[ "${NEEDLE_AGENT[provider]}" == "openai" ]]; then
        test_pass
    else
        test_fail "Adapter fields mismatch"
    fi
else
    test_fail "Failed to load adapter"
fi

test_case "Loads aider-ollama-deepseek adapter"
if _needle_load_agent "aider-ollama-deepseek" 2>/dev/null; then
    if [[ "${NEEDLE_AGENT[runner]}" == "aider" ]] && \
       [[ "${NEEDLE_AGENT[provider]}" == "ollama" ]]; then
        test_pass
    else
        test_fail "Adapter fields mismatch"
    fi
else
    test_fail "Failed to load adapter"
fi

# =============================================================================
# SECTION 2: Custom Adapter Loading Tests
# =============================================================================

echo ""
echo "=== Custom Adapter Loading Tests ==="

# Create custom adapter in workspace
mkdir -p "$TEST_DIR/.needle/agents"

test_case "Loads custom workspace adapter"
cat > "$TEST_DIR/.needle/agents/custom-test.yaml" << 'EOF'
name: custom-test
description: Custom test adapter
version: "1.0"
runner: bash
provider: test
model: test-model

invoke: |
  echo "Custom adapter executed"
  echo "PROMPT: ${PROMPT}"

input:
  method: heredoc

output:
  format: text
  token_pattern: "tokens: ([0-9]+)/([0-9]+)"

limits:
  requests_per_minute: 30
  max_concurrent: 2

cost:
  type: unlimited
EOF

pushd "$TEST_DIR" >/dev/null
if _needle_load_agent "custom-test" 2>/dev/null; then
    if [[ "${NEEDLE_AGENT[name]}" == "custom-test" ]] && \
       [[ "${NEEDLE_AGENT[requests_per_minute]}" == "30" ]] && \
       [[ "${NEEDLE_AGENT[max_concurrent]}" == "2" ]]; then
        test_pass
    else
        test_fail "Custom adapter fields mismatch"
    fi
else
    test_fail "Failed to load custom adapter"
fi
popd >/dev/null

test_case "Custom adapter with args input method"
cat > "$TEST_DIR/.needle/agents/args-adapter.yaml" << 'EOF'
name: args-adapter
description: Adapter using args input method
version: "1.0"
runner: echo
provider: test
model: test

invoke: |
  echo "${PROMPT}"

input:
  method: args

output:
  format: text

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

pushd "$TEST_DIR" >/dev/null
if _needle_load_agent "args-adapter" 2>/dev/null; then
    if [[ "${NEEDLE_AGENT[input_method]}" == "args" ]]; then
        test_pass
    else
        test_fail "Expected args input method, got '${NEEDLE_AGENT[input_method]}'"
    fi
else
    test_fail "Failed to load args adapter"
fi
popd >/dev/null

test_case "Custom adapter with stdin input method"
cat > "$TEST_DIR/.needle/agents/stdin-adapter.yaml" << 'EOF'
name: stdin-adapter
description: Adapter using stdin input method
version: "1.0"
runner: cat
provider: test
model: test

invoke: cat

input:
  method: stdin

output:
  format: text

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

pushd "$TEST_DIR" >/dev/null
if _needle_load_agent "stdin-adapter" 2>/dev/null; then
    if [[ "${NEEDLE_AGENT[input_method]}" == "stdin" ]]; then
        test_pass
    else
        test_fail "Expected stdin input method"
    fi
else
    test_fail "Failed to load stdin adapter"
fi
popd >/dev/null

test_case "Custom adapter with file input method"
cat > "$TEST_DIR/.needle/agents/file-adapter.yaml" << 'EOF'
name: file-adapter
description: Adapter using file input method
version: "1.0"
runner: cat
provider: test
model: test

invoke: cat ${PROMPT_FILE}

input:
  method: file
  file_path: /tmp/needle-prompt.txt

output:
  format: text

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

pushd "$TEST_DIR" >/dev/null
if _needle_load_agent "file-adapter" 2>/dev/null; then
    if [[ "${NEEDLE_AGENT[input_method]}" == "file" ]] && \
       [[ "${NEEDLE_AGENT[input_file_path]}" == "/tmp/needle-prompt.txt" ]]; then
        test_pass
    else
        test_fail "Expected file input method with path"
    fi
else
    test_fail "Failed to load file adapter"
fi
popd >/dev/null

# =============================================================================
# SECTION 3: Mock Agent Invocation Tests
# =============================================================================

echo ""
echo "=== Mock Agent Invocation Tests ==="

# Create mock agent that simulates CLI behavior
test_case "Mock agent: heredoc method invocation"
cat > "$TEST_DIR/.needle/agents/mock-heredoc.yaml" << 'EOF'
name: mock-heredoc
description: Mock agent using heredoc
version: "1.0"
runner: bash
provider: mock
model: mock-model

invoke: |
  echo "MOCK OUTPUT: Task completed"
  echo "TOKENS: input=500 output=250"

input:
  method: heredoc

output:
  format: text
  token_pattern: "input=([0-9]+).*output=([0-9]+)"

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

pushd "$TEST_DIR" >/dev/null
result=$(_needle_dispatch_agent "mock-heredoc" "$TEST_DIR" "Test prompt" "nd-test-1" "Test Title" 0)
exit_code=$(echo "$result" | cut -d'|' -f1)
output_file=$(echo "$result" | cut -d'|' -f3)
output=$(cat "$output_file" 2>/dev/null)
rm -f "$output_file" 2>/dev/null

if [[ "$exit_code" == "0" ]] && [[ "$output" == *"MOCK OUTPUT"* ]] && [[ "$output" == *"TOKENS"* ]]; then
    test_pass
else
    test_fail "Expected mock output, got exit=$exit_code output=$output"
fi
popd >/dev/null

test_case "Mock agent: stdin method invocation"
cat > "$TEST_DIR/.needle/agents/mock-stdin.yaml" << 'EOF'
name: mock-stdin
description: Mock agent using stdin
version: "1.0"
runner: bash
provider: mock
model: mock-model

invoke: cat && echo "---STDIN_RECEIVED---"

input:
  method: stdin

output:
  format: text

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

pushd "$TEST_DIR" >/dev/null
result=$(_needle_dispatch_agent "mock-stdin" "$TEST_DIR" "StdinPromptTest" "nd-test-2" "Test" 0)
exit_code=$(echo "$result" | cut -d'|' -f1)
output_file=$(echo "$result" | cut -d'|' -f3)
output=$(cat "$output_file" 2>/dev/null)
rm -f "$output_file" 2>/dev/null

if [[ "$exit_code" == "0" ]] && [[ "$output" == *"StdinPromptTest"* ]] && [[ "$output" == *"STDIN_RECEIVED"* ]]; then
    test_pass
else
    test_fail "Expected stdin prompt in output, got exit=$exit_code output=$output"
fi
popd >/dev/null

test_case "Mock agent: args method invocation"
cat > "$TEST_DIR/.needle/agents/mock-args.yaml" << 'EOF'
name: mock-args
description: Mock agent using args
version: "1.0"
runner: bash
provider: mock
model: mock-model

invoke: |
  echo "Args received ${PROMPT}"

input:
  method: args

output:
  format: text

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

pushd "$TEST_DIR" >/dev/null
result=$(_needle_dispatch_agent "mock-args" "$TEST_DIR" "ArgsPrompt123" "nd-test-3" "Test" 0)
exit_code=$(echo "$result" | cut -d'|' -f1)
output_file=$(echo "$result" | cut -d'|' -f3)
output=$(cat "$output_file" 2>/dev/null)
rm -f "$output_file" 2>/dev/null

if [[ "$exit_code" == "0" ]] && [[ "$output" == *"ArgsPrompt123"* ]]; then
    test_pass
else
    test_fail "Expected args prompt in output, got exit=$exit_code output=$output"
fi
popd >/dev/null

test_case "Mock agent: file method invocation"
cat > "$TEST_DIR/.needle/agents/mock-file.yaml" << 'EOF'
name: mock-file
description: Mock agent using file
version: "1.0"
runner: bash
provider: mock
model: mock-model

invoke: cat ${PROMPT_FILE} && echo "---FILE_READ---"

input:
  method: file
  file_path: /tmp/needle-test-prompt.txt

output:
  format: text

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

pushd "$TEST_DIR" >/dev/null
result=$(_needle_dispatch_agent "mock-file" "$TEST_DIR" "FilePromptContent" "nd-test-4" "Test" 0)
exit_code=$(echo "$result" | cut -d'|' -f1)
output_file=$(echo "$result" | cut -d'|' -f3)
output=$(cat "$output_file" 2>/dev/null)
rm -f "$output_file" 2>/dev/null

if [[ "$exit_code" == "0" ]] && [[ "$output" == *"FilePromptContent"* ]] && [[ "$output" == *"FILE_READ"* ]]; then
    test_pass
else
    test_fail "Expected file prompt in output, got exit=$exit_code output=$output"
fi
popd >/dev/null

# =============================================================================
# SECTION 4: Token Extraction Tests with Mock Output
# =============================================================================

echo ""
echo "=== Token Extraction Tests ==="

test_case "Extract tokens from JSON output (Claude style)"
cat > "$TEST_DIR/claude-output.json" << 'EOF'
{
  "type": "message",
  "id": "msg_123",
  "content": [{"type": "text", "text": "Response text"}],
  "usage": {
    "input_tokens": 1234,
    "output_tokens": 567
  }
}
EOF
result=$(_needle_extract_tokens_json "$TEST_DIR/claude-output.json")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "1234" ]] && [[ "$output" == "567" ]]; then
    test_pass
else
    test_fail "Expected 1234|567, got $input|$output"
fi

test_case "Extract tokens from JSON output (OpenAI style)"
cat > "$TEST_DIR/openai-output.json" << 'EOF'
{
  "id": "chatcmpl-123",
  "choices": [{"message": {"content": "Response"}}],
  "usage": {
    "prompt_tokens": 2500,
    "completion_tokens": 1200
  }
}
EOF
result=$(_needle_extract_tokens_json "$TEST_DIR/openai-output.json")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "2500" ]] && [[ "$output" == "1200" ]]; then
    test_pass
else
    test_fail "Expected 2500|1200, got $input|$output"
fi

test_case "Extract tokens from text output with pattern"
cat > "$TEST_DIR/text-output.txt" << 'EOF'
Task completed successfully.
Token usage: input=3000 output=1500
EOF
result=$(_needle_extract_tokens_text "$TEST_DIR/text-output.txt" "input=([0-9]+).*output=([0-9]+)")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "3000" ]] && [[ "$output" == "1500" ]]; then
    test_pass
else
    test_fail "Expected 3000|1500, got $input|$output"
fi

test_case "Token extraction with agent config integration"
pushd "$TEST_DIR" >/dev/null
cat > "$TEST_DIR/.needle/agents/token-test.yaml" << 'EOF'
name: token-test
description: Token test agent
version: "1.0"
runner: echo
provider: test
model: test

invoke: echo "test"

input:
  method: heredoc

output:
  format: json

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

cat > "$TEST_DIR/agent-output.log" << 'EOF'
{"input_tokens": 9999, "output_tokens": 5555}
EOF

result=$(_needle_extract_tokens "token-test" "$TEST_DIR/agent-output.log")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)

if [[ "$input" == "9999" ]] && [[ "$output" == "5555" ]]; then
    test_pass
else
    test_fail "Expected 9999|5555 via agent config, got $input|$output"
fi
popd >/dev/null

test_case "Token extraction fallback for missing data"
cat > "$TEST_DIR/empty-output.txt" << 'EOF'
No token information here
EOF
result=$(_needle_extract_tokens_text_autodetect "$TEST_DIR/empty-output.txt")
if [[ "$result" == "0|0" ]]; then
    test_pass
else
    test_fail "Expected 0|0 for missing tokens, got $result"
fi

# =============================================================================
# SECTION 5: Prompt Escaping Integration Tests
# =============================================================================

echo ""
echo "=== Prompt Escaping Integration Tests ==="

test_case "Prompt with special chars preserved in heredoc"
pushd "$TEST_DIR" >/dev/null
cat > "$TEST_DIR/.needle/agents/escape-test.yaml" << 'EOF'
name: escape-test
description: Escape test agent
version: "1.0"
runner: bash
provider: test
model: test

invoke: |
  cat <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT

input:
  method: heredoc

output:
  format: text

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

prompt='Test with $pecial `backtick` "quotes" and '\''single'\'' quotes'
result=$(_needle_dispatch_agent "escape-test" "$TEST_DIR" "$prompt" "nd-escape-1" "Test" 0)
output_file=$(echo "$result" | cut -d'|' -f3)
output=$(cat "$output_file" 2>/dev/null)
rm -f "$output_file" 2>/dev/null

# Heredoc should preserve special chars literally
if [[ "$output" == *'$pecial'* ]] && [[ "$output" == *'`backtick`'* ]] && [[ "$output" == *'"quotes"'* ]]; then
    test_pass
else
    test_fail "Special chars not preserved: $output"
fi
popd >/dev/null

test_case "Prompt with newlines preserved"
pushd "$TEST_DIR" >/dev/null
prompt=$'Line 1\nLine 2\nLine 3'
result=$(_needle_dispatch_agent "escape-test" "$TEST_DIR" "$prompt" "nd-escape-2" "Test" 0)
output_file=$(echo "$result" | cut -d'|' -f3)
output=$(cat "$output_file" 2>/dev/null)
rm -f "$output_file" 2>/dev/null

if [[ "$output" == *"Line 1"* ]] && [[ "$output" == *"Line 2"* ]] && [[ "$output" == *"Line 3"* ]]; then
    test_pass
else
    test_fail "Newlines not preserved: $output"
fi
popd >/dev/null

test_case "Prompt escaping for args method"
pushd "$TEST_DIR" >/dev/null
cat > "$TEST_DIR/.needle/agents/args-escape.yaml" << 'EOF'
name: args-escape
description: Args escape test
version: "1.0"
runner: bash
provider: test
model: test

invoke: echo "${PROMPT}"

input:
  method: args

output:
  format: text

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

prompt='Test $VAR and `cmd`'
result=$(_needle_dispatch_agent "args-escape" "$TEST_DIR" "$prompt" "nd-escape-3" "Test" 0)
exit_code=$(echo "$result" | cut -d'|' -f1)
output_file=$(echo "$result" | cut -d'|' -f3)
output=$(cat "$output_file" 2>/dev/null)
rm -f "$output_file" 2>/dev/null

# Args method should escape special chars so they print literally
if [[ "$exit_code" == "0" ]] && [[ "$output" == *'$VAR'* ]] && [[ "$output" == *'`cmd`'* ]]; then
    test_pass
else
    test_fail "Special chars not properly escaped: exit=$exit_code output=$output"
fi
popd >/dev/null

# =============================================================================
# SECTION 6: Error Handling Tests
# =============================================================================

echo ""
echo "=== Error Handling Tests ==="

test_case "Error: nonexistent agent"
pushd "$TEST_DIR" >/dev/null
result=$(_needle_dispatch_agent "nonexistent-agent-xyz" "$TEST_DIR" "prompt" "nd-err-1" "Test" 0 2>/dev/null)
exit_code=$?
if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Expected failure for nonexistent agent"
fi
popd >/dev/null

test_case "Error: missing workspace"
result=$(_needle_dispatch_agent "claude-anthropic-sonnet" "" "prompt" "nd-err-2" "Test" 0 2>/dev/null)
exit_code=$?
if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Expected failure for missing workspace"
fi

test_case "Error: missing prompt"
result=$(_needle_dispatch_agent "claude-anthropic-sonnet" "$TEST_DIR" "" "nd-err-3" "Test" 0 2>/dev/null)
exit_code=$?
if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Expected failure for missing prompt"
fi

test_case "Error: malformed YAML adapter"
mkdir -p "$TEST_DIR/.needle/agents"
cat > "$TEST_DIR/.needle/agents/malformed.yaml" << 'EOF'
name: malformed
description: [broken
  yaml structure
    unclosed [
EOF

pushd "$TEST_DIR" >/dev/null
# Malformed YAML should either fail to load or have incomplete fields
_needle_load_agent "malformed" 2>/dev/null
load_result=$?
# Check if either: 1) load failed, or 2) critical fields are missing
if [[ $load_result -ne 0 ]] || [[ -z "${NEEDLE_AGENT[runner]:-}" ]] || [[ -z "${NEEDLE_AGENT[invoke]:-}" ]]; then
    test_pass
else
    test_fail "Malformed YAML should fail or have missing fields"
fi
popd >/dev/null

test_case "Error: adapter missing required fields"
mkdir -p "$TEST_DIR/.needle/agents"
cat > "$TEST_DIR/.needle/agents/incomplete.yaml" << 'EOF'
name: incomplete
description: Missing runner and invoke
EOF

pushd "$TEST_DIR" >/dev/null
if ! _needle_validate_agent "incomplete" 2>/dev/null; then
    test_pass
else
    test_fail "Expected validation failure for incomplete adapter"
fi
popd >/dev/null

test_case "Error: invalid input method"
mkdir -p "$TEST_DIR/.needle/agents"
cat > "$TEST_DIR/.needle/agents/bad-method.yaml" << 'EOF'
name: bad-method
description: Invalid input method
runner: bash
invoke: echo test
input:
  method: invalid_method_xyz
EOF

pushd "$TEST_DIR" >/dev/null
if ! _needle_validate_agent "bad-method" 2>/dev/null; then
    test_pass
else
    test_fail "Expected validation failure for invalid input method"
fi
popd >/dev/null

# =============================================================================
# SECTION 7: Adapter Discovery Tests
# =============================================================================

echo ""
echo "=== Adapter Discovery Tests ==="

test_case "List available agents includes built-in"
result=$(_needle_list_available_agents)
if [[ "$result" == *"claude-anthropic-sonnet"* ]] && \
   [[ "$result" == *"opencode-alibaba-qwen"* ]]; then
    test_pass
else
    test_fail "Expected built-in agents in list"
fi

test_case "List available agents as JSON"
result=$(_needle_list_available_agents --json)
if [[ "$result" == "["*"]" ]] && [[ "$result" == *'"claude-anthropic-sonnet"'* ]]; then
    test_pass
else
    test_fail "Expected JSON array with agents"
fi

test_case "Find agent config path"
result=$(_needle_find_agent_config "claude-anthropic-sonnet")
if [[ -n "$result" ]] && [[ -f "$result" ]]; then
    test_pass
else
    test_fail "Expected valid config path"
fi

test_case "Workspace adapter overrides built-in"
mkdir -p "$TEST_DIR/.needle/agents"
cat > "$TEST_DIR/.needle/agents/claude-anthropic-sonnet.yaml" << 'EOF'
name: claude-anthropic-sonnet
description: Custom override
runner: custom-runner
invoke: custom command
EOF

pushd "$TEST_DIR" >/dev/null
result=$(_needle_find_agent_config "claude-anthropic-sonnet")
if [[ "$result" == *".needle/agents/claude-anthropic-sonnet.yaml"* ]]; then
    test_pass
else
    test_fail "Expected workspace config to take precedence, got $result"
fi
popd >/dev/null

# Clean up the override so it doesn't affect subsequent tests
rm -f "$TEST_DIR/.needle/agents/claude-anthropic-sonnet.yaml"

# =============================================================================
# SECTION 8: Cost Calculation Integration Tests
# =============================================================================

echo ""
echo "=== Cost Calculation Integration Tests ==="

test_case "Calculate cost for pay_per_token agent"
# Debug: Check if calculate_cost function exists and what it returns
# First ensure the agent is loaded
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null || true

# Get cost config for debugging
cost_config=$(_needle_get_agent_cost_config "claude-anthropic-sonnet" 2>/dev/null)
cost_type=$(_needle_get_cost_field "claude-anthropic-sonnet" "type" 2>/dev/null)
input_rate=$(_needle_get_cost_field "claude-anthropic-sonnet" "input_per_1k" 2>/dev/null)
output_rate=$(_needle_get_cost_field "claude-anthropic-sonnet" "output_per_1k" 2>/dev/null)

# Calculate cost
cost=$(calculate_cost "claude-anthropic-sonnet" 10000 5000 2>/dev/null)

# Debug output (only shown on failure)
debug_info="config=$cost_config type=$cost_type in_rate=$input_rate out_rate=$output_rate cost=$cost"

# 10000 * 0.003/1000 = 0.03, 5000 * 0.015/1000 = 0.075, total = 0.105
if [[ -n "$cost" ]] && [[ "$cost" != "0" ]] && [[ "$cost" != "0.00" ]] && [[ "$cost" != "0.000000" ]]; then
    test_pass
else
    test_fail "Expected non-zero cost, got $cost. Debug: $debug_info"
fi

test_case "Calculate cost for unlimited agent"
cost=$(calculate_cost "opencode-ollama-deepseek" 10000 5000 2>/dev/null)
if [[ "$cost" == "0.00" ]]; then
    test_pass
else
    test_fail "Expected 0.00 for unlimited agent, got $cost"
fi

test_case "Cost config retrieval"
config=$(_needle_get_agent_cost_config "claude-anthropic-sonnet" 2>/dev/null)
if [[ "$config" == *"pay_per_token"* ]] || [[ "$config" == *"input_per_1k"* ]]; then
    test_pass
else
    test_fail "Expected cost config, got $config"
fi

# =============================================================================
# SECTION 9: Exit Code Classification Tests
# =============================================================================

echo ""
echo "=== Exit Code Classification Tests ==="

test_case "Classify success exit code"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
if _needle_is_success_exit_code 0; then
    test_pass
else
    test_fail "Expected 0 to be success"
fi

test_case "Classify retry exit code"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
if _needle_is_retry_exit_code 1; then
    test_pass
else
    test_fail "Expected 1 to be retry"
fi

test_case "Classify fail exit code"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
if _needle_is_fail_exit_code 137; then
    test_pass
else
    test_fail "Expected 137 to be fail"
fi

test_case "Exit code classification returns correct category"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
result=$(_needle_classify_exit_code 0)
if [[ "$result" == "success" ]]; then
    test_pass
else
    test_fail "Expected 'success', got '$result'"
fi

# =============================================================================
# SECTION 10: Timeout Handling Tests
# =============================================================================

echo ""
echo "=== Timeout Handling Tests ==="

test_case "Timeout terminates long-running command"
pushd "$TEST_DIR" >/dev/null
cat > "$TEST_DIR/.needle/agents/slow-agent.yaml" << 'EOF'
name: slow-agent
description: Slow running agent
version: "1.0"
runner: bash
provider: test
model: test

invoke: sleep 10 && echo "Should not reach here"

input:
  method: heredoc

output:
  format: text

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

start_time=$(date +%s)
result=$(_needle_dispatch_agent "slow-agent" "$TEST_DIR" "prompt" "nd-timeout" "Test" 2)
end_time=$(date +%s)
duration=$((end_time - start_time))
exit_code=$(echo "$result" | cut -d'|' -f1)
output_file=$(echo "$result" | cut -d'|' -f3)
rm -f "$output_file" 2>/dev/null

# timeout returns 124 when it kills the process
if [[ $exit_code -eq 124 ]] && [[ $duration -lt 5 ]]; then
    test_pass
else
    test_fail "Expected timeout exit 124 in <5s, got exit=$exit_code in ${duration}s"
fi
popd >/dev/null

# =============================================================================
# Summary
# =============================================================================
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
