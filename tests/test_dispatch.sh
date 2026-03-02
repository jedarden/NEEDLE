#!/usr/bin/env bash
# Tests for NEEDLE agent dispatch module (src/agent/dispatch.sh)

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

# ============ Template Rendering Tests ============

test_case "_needle_render_invoke substitutes WORKSPACE"
template='echo ${WORKSPACE}'
result=$(_needle_render_invoke "$template" "/home/user/project" "prompt" "nd-100" "Test")
if [[ "$result" == "echo /home/user/project" ]]; then
    test_pass
else
    test_fail "Expected 'echo /home/user/project', got '$result'"
fi

test_case "_needle_render_invoke substitutes BEAD_ID"
template='echo ${BEAD_ID}'
result=$(_needle_render_invoke "$template" "/workspace" "prompt" "nd-100" "Test")
if [[ "$result" == "echo nd-100" ]]; then
    test_pass
else
    test_fail "Expected 'echo nd-100', got '$result'"
fi

test_case "_needle_render_invoke substitutes BEAD_TITLE"
template='echo ${BEAD_TITLE}'
result=$(_needle_render_invoke "$template" "/workspace" "prompt" "nd-100" "My Test Title")
if [[ "$result" == "echo My Test Title" ]]; then
    test_pass
else
    test_fail "Expected 'echo My Test Title', got '$result'"
fi

test_case "_needle_render_invoke substitutes PROMPT"
template='echo ${PROMPT}'
result=$(_needle_render_invoke "$template" "/workspace" "Hello World" "nd-100" "Test")
if [[ "$result" == "echo Hello World" ]]; then
    test_pass
else
    test_fail "Expected 'echo Hello World', got '$result'"
fi

test_case "_needle_render_invoke substitutes all variables"
template='cd ${WORKSPACE} && echo "${BEAD_ID}: ${BEAD_TITLE}" && cat <<EOF\n${PROMPT}\nEOF'
result=$(_needle_render_invoke "$template" "/home/project" "Fix bug" "nd-1hr" "Fix Issue")
if [[ "$result" == *"/home/project"* ]] && [[ "$result" == *"nd-1hr"* ]] && [[ "$result" == *"Fix Issue"* ]] && [[ "$result" == *"Fix bug"* ]]; then
    test_pass
else
    test_fail "Expected all variables substituted, got '$result'"
fi

test_case "_needle_render_invoke handles multiline template"
template='cd ${WORKSPACE}
echo "${BEAD_ID}"
echo "${PROMPT}"'
result=$(_needle_render_invoke "$template" "/workspace" "test prompt" "nd-100" "Title")
if [[ "$result" == *"cd /workspace"* ]] && [[ "$result" == *"nd-100"* ]] && [[ "$result" == *"test prompt"* ]]; then
    test_pass
else
    test_fail "Expected multiline substitution, got '$result'"
fi

# ============ Prompt Escaping Tests ============

test_case "_needle_escape_prompt_for_args escapes double quotes"
result=$(_needle_escape_prompt_for_args 'Say "hello"')
if [[ "$result" == 'Say \"hello\"' ]]; then
    test_pass
else
    test_fail "Expected escaped quotes, got '$result'"
fi

test_case "_needle_escape_prompt_for_args escapes dollar signs"
result=$(_needle_escape_prompt_for_args 'Use $variable')
if [[ "$result" == 'Use \$variable' ]]; then
    test_pass
else
    test_fail "Expected escaped dollar sign, got '$result'"
fi

test_case "_needle_escape_prompt_for_args escapes backticks"
result=$(_needle_escape_prompt_for_args 'Run `command`')
if [[ "$result" == 'Run \`command\`' ]]; then
    test_pass
else
    test_fail "Expected escaped backtick, got '$result'"
fi

test_case "_needle_escape_prompt_for_args escapes backslashes"
result=$(_needle_escape_prompt_for_args 'Path: C:\Users')
if [[ "$result" == 'Path: C:\\Users' ]]; then
    test_pass
else
    test_fail "Expected escaped backslash, got '$result'"
fi

# ============ Args Method Rendering Tests ============

test_case "_needle_render_invoke_args escapes special chars in prompt"
template='echo "${PROMPT}"'
result=$(_needle_render_invoke_args "$template" "/workspace" 'Test $var "quoted" `cmd`' "nd-100" "Test")
if [[ "$result" == *'\$var'* ]] && [[ "$result" == *'\"quoted\"'* ]] && [[ "$result" == *'\`cmd\`'* ]]; then
    test_pass
else
    test_fail "Expected escaped special chars, got '$result'"
fi

# ============ Time Utility Tests ============

test_case "_needle_get_time_ms returns numeric value"
result=$(_needle_get_time_ms)
if [[ "$result" =~ ^[0-9]+$ ]]; then
    test_pass
else
    test_fail "Expected numeric value, got '$result'"
fi

# ============ Exit Code Classification Tests ============

test_case "_needle_is_success_exit_code returns true for 0"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
if _needle_is_success_exit_code 0; then
    test_pass
else
    test_fail "Expected exit code 0 to be success"
fi

test_case "_needle_is_retry_exit_code returns true for 1"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
if _needle_is_retry_exit_code 1; then
    test_pass
else
    test_fail "Expected exit code 1 to be retry"
fi

test_case "_needle_is_fail_exit_code returns true for 137"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
if _needle_is_fail_exit_code 137; then
    test_pass
else
    test_fail "Expected exit code 137 to be fail"
fi

test_case "_needle_classify_exit_code returns correct classification"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
result=$(_needle_classify_exit_code 0)
if [[ "$result" == "success" ]]; then
    test_pass
else
    test_fail "Expected 'success', got '$result'"
fi

test_case "_needle_classify_exit_code returns retry for 1"
_needle_load_agent "claude-anthropic-sonnet" 2>/dev/null
result=$(_needle_classify_exit_code 1)
if [[ "$result" == "retry" ]]; then
    test_pass
else
    test_fail "Expected 'retry', got '$result'"
fi

# ============ Dispatch Result Parsing Tests ============

test_case "_needle_parse_dispatch_result parses result string"
result="0|1234|/tmp/output.log"
NEEDLE_DISPATCH_exit_code=""
NEEDLE_DISPATCH_duration=""
NEEDLE_DISPATCH_output_file=""
_needle_parse_dispatch_result "$result" "NEEDLE_DISPATCH"
if [[ "$NEEDLE_DISPATCH_exit_code" == "0" ]] && \
   [[ "$NEEDLE_DISPATCH_duration" == "1234" ]] && \
   [[ "$NEEDLE_DISPATCH_output_file" == "/tmp/output.log" ]]; then
    test_pass
else
    test_fail "Expected parsed values, got exit=$NEEDLE_DISPATCH_exit_code dur=$NEEDLE_DISPATCH_duration out=$NEEDLE_DISPATCH_output_file"
fi

# ============ Heredoc Method Tests ============

test_case "_needle_dispatch_heredoc executes simple command"
output_file=$(mktemp)
rendered='echo "Hello from heredoc"'
_needle_dispatch_heredoc "$rendered" "$output_file" 0
exit_code=$?
output=$(cat "$output_file")
rm -f "$output_file"
if [[ $exit_code -eq 0 ]] && [[ "$output" == "Hello from heredoc" ]]; then
    test_pass
else
    test_fail "Expected exit 0 and output, got exit=$exit_code output=$output"
fi

test_case "_needle_dispatch_heredoc captures stderr"
output_file=$(mktemp)
rendered='echo "stdout" && echo "stderr" >&2'
_needle_dispatch_heredoc "$rendered" "$output_file" 0
output=$(cat "$output_file")
rm -f "$output_file"
if [[ "$output" == *"stdout"* ]] && [[ "$output" == *"stderr"* ]]; then
    test_pass
else
    test_fail "Expected both stdout and stderr, got '$output'"
fi

test_case "_needle_dispatch_heredoc respects timeout"
output_file=$(mktemp)
rendered='sleep 5'
start=$(date +%s)
_needle_dispatch_heredoc "$rendered" "$output_file" 1  # 1 second timeout
exit_code=$?
end=$(date +%s)
rm -f "$output_file"
duration=$((end - start))
if [[ $exit_code -eq 124 ]] && [[ $duration -lt 3 ]]; then
    test_pass
else
    test_fail "Expected timeout exit 124 in <3s, got exit=$exit_code in ${duration}s"
fi

# ============ Stdin Method Tests ============

test_case "_needle_dispatch_stdin pipes prompt to command"
output_file=$(mktemp)
prompt="Hello from stdin"
invoke_cmd='cat'
_needle_dispatch_stdin "$invoke_cmd" "$prompt" "$output_file" 0
exit_code=$?
output=$(cat "$output_file")
rm -f "$output_file"
if [[ $exit_code -eq 0 ]] && [[ "$output" == "$prompt" ]]; then
    test_pass
else
    test_fail "Expected prompt in output, got '$output'"
fi

# ============ File Method Tests ============

test_case "_needle_dispatch_file writes prompt to file"
output_file=$(mktemp)
prompt="Hello from file"
file_path="$TEST_DIR/prompt-input.txt"
invoke_cmd='cat ${PROMPT_FILE}'
_needle_dispatch_file "$invoke_cmd" "$prompt" "$file_path" "$output_file" 0
exit_code=$?
output=$(cat "$output_file")
rm -f "$output_file"
if [[ $exit_code -eq 0 ]] && [[ "$output" == "$prompt" ]]; then
    test_pass
else
    test_fail "Expected prompt in output, got '$output'"
fi

test_case "_needle_dispatch_file cleans up input file"
output_file=$(mktemp)
prompt="Test cleanup"
file_path="$TEST_DIR/prompt-to-cleanup.txt"
invoke_cmd='cat ${PROMPT_FILE}'
_needle_dispatch_file "$invoke_cmd" "$prompt" "$file_path" "$output_file" 0
rm -f "$output_file"
if [[ ! -f "$file_path" ]]; then
    test_pass
else
    test_fail "Expected input file to be cleaned up"
fi

# ============ Args Method Tests ============

test_case "_needle_dispatch_args executes with escaped prompt"
output_file=$(mktemp)
rendered='echo "Testing args: \$variable"'
_needle_dispatch_args "$rendered" "$output_file" 0
exit_code=$?
output=$(cat "$output_file")
rm -f "$output_file"
if [[ $exit_code -eq 0 ]] && [[ "$output" == "Testing args: \$variable" ]]; then
    test_pass
else
    test_fail "Expected escaped output, got '$output'"
fi

# ============ Test Agent for Full Dispatch ============

# Create a test agent that uses bash echo for reliable testing
mkdir -p "$TEST_DIR/.needle/agents"
cat > "$TEST_DIR/.needle/agents/test-echo.yaml" << 'EOF'
name: test-echo
description: Test agent using echo
version: "1.0"
runner: bash
provider: test
model: echo

invoke: |
  echo "Workspace: ${WORKSPACE}"
  echo "Bead ID: ${BEAD_ID}"
  echo "Title: ${BEAD_TITLE}"
  cat <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT

input:
  method: heredoc

output:
  format: text
  success_codes: [0]
  retry_codes: [1]
  fail_codes: [2]

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

# Change to test directory for agent discovery
pushd "$TEST_DIR" >/dev/null

test_case "_needle_dispatch_agent returns result string format"
result=$(_needle_dispatch_agent "test-echo" "$TEST_DIR" "Test prompt content" "nd-test" "Test Title" 0)
if [[ "$result" =~ ^[0-9]+\|[0-9]+\|.*/needle-dispatch-nd-test-.*\.log$ ]]; then
    test_pass
else
    test_fail "Expected result format 'exit|duration|file', got '$result'"
fi

test_case "_needle_dispatch_agent captures output correctly"
result=$(_needle_dispatch_agent "test-echo" "$TEST_DIR" "UniquePromptText123" "nd-test" "Test Title" 0)
output_file=$(echo "$result" | cut -d'|' -f3)
output=$(cat "$output_file")
rm -f "$output_file"
if [[ "$output" == *"UniquePromptText123"* ]]; then
    test_pass
else
    test_fail "Expected prompt in output, got '$output'"
fi

test_case "_needle_dispatch_agent captures workspace and bead info"
result=$(_needle_dispatch_agent "test-echo" "$TEST_DIR" "prompt" "nd-xyz" "MyBeadTitle" 0)
output_file=$(echo "$result" | cut -d'|' -f3)
output=$(cat "$output_file")
rm -f "$output_file"
if [[ "$output" == *"$TEST_DIR"* ]] && [[ "$output" == *"nd-xyz"* ]] && [[ "$output" == *"MyBeadTitle"* ]]; then
    test_pass
else
    test_fail "Expected workspace and bead info in output, got '$output'"
fi

test_case "_needle_dispatch_agent measures duration"
result=$(_needle_dispatch_agent "test-echo" "$TEST_DIR" "prompt" "nd-test" "Title" 0)
duration=$(echo "$result" | cut -d'|' -f2)
output_file=$(echo "$result" | cut -d'|' -f3)
rm -f "$output_file"
if [[ "$duration" =~ ^[0-9]+$ ]] && [[ "$duration" -ge 0 ]]; then
    test_pass
else
    test_fail "Expected numeric duration >= 0, got '$duration'"
fi

test_case "_needle_dispatch_agent returns zero exit code on success"
result=$(_needle_dispatch_agent "test-echo" "$TEST_DIR" "prompt" "nd-test" "Title" 0)
exit_code=$(echo "$result" | cut -d'|' -f1)
output_file=$(echo "$result" | cut -d'|' -f3)
rm -f "$output_file"
if [[ "$exit_code" == "0" ]]; then
    test_pass
else
    test_fail "Expected exit code 0, got '$exit_code'"
fi

# Test with args method agent
cat > "$TEST_DIR/.needle/agents/test-args.yaml" << 'EOF'
name: test-args
description: Test agent using args method
version: "1.0"
runner: bash
provider: test
model: echo

invoke: |
  echo "Prompt was: ${PROMPT}"

input:
  method: args

output:
  format: text
  success_codes: [0]

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

test_case "_needle_dispatch_agent handles args method"
result=$(_needle_dispatch_agent "test-args" "$TEST_DIR" "ArgsPromptTest" "nd-args" "Args Test" 0)
exit_code=$(echo "$result" | cut -d'|' -f1)
output_file=$(echo "$result" | cut -d'|' -f3)
output=$(cat "$output_file")
rm -f "$output_file"
if [[ "$exit_code" == "0" ]] && [[ "$output" == *"ArgsPromptTest"* ]]; then
    test_pass
else
    test_fail "Expected args method to work, got exit=$exit_code output=$output"
fi

popd >/dev/null

# ============ Error Handling Tests ============

test_case "_needle_dispatch_agent fails for nonexistent agent"
result=$(_needle_dispatch_agent "nonexistent-agent-xyz" "$TEST_DIR" "prompt" "nd-test" "Title" 0 2>/dev/null)
exit_code=$?
if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Expected failure for nonexistent agent"
fi

test_case "_needle_dispatch_agent fails without workspace"
result=$(_needle_dispatch_agent "claude-anthropic-sonnet" "" "prompt" "nd-test" "Title" 0 2>/dev/null)
exit_code=$?
if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Expected failure without workspace"
fi

test_case "_needle_dispatch_agent fails without prompt"
result=$(_needle_dispatch_agent "claude-anthropic-sonnet" "$TEST_DIR" "" "nd-test" "Title" 0 2>/dev/null)
exit_code=$?
if [[ $exit_code -ne 0 ]]; then
    test_pass
else
    test_fail "Expected failure without prompt"
fi

# ============ Special Character Handling Tests ============

pushd "$TEST_DIR" >/dev/null

test_case "_needle_dispatch_agent handles quotes in prompt"
result=$(_needle_dispatch_agent "test-echo" "$TEST_DIR" 'Say "hello" and '\''goodbye'\''' "nd-test" "Title" 0)
output_file=$(echo "$result" | cut -d'|' -f3)
exit_code=$(echo "$result" | cut -d'|' -f1)
output=$(cat "$output_file")
rm -f "$output_file"
# Heredoc method should preserve quotes literally
if [[ "$output" == *'"hello"'* ]] && [[ "$output" == *"goodbye"* ]]; then
    test_pass
else
    test_fail "Expected quotes preserved, got '$output'"
fi

test_case "_needle_dispatch_agent handles dollar signs in prompt"
result=$(_needle_dispatch_agent "test-echo" "$TEST_DIR" 'Use $HOME and $PATH here' "nd-test" "Title" 0)
output_file=$(echo "$result" | cut -d'|' -f3)
output=$(cat "$output_file")
rm -f "$output_file"
if [[ "$output" == *'$HOME'* ]] && [[ "$output" == *'$PATH'* ]]; then
    test_pass
else
    test_fail "Expected dollar signs preserved, got '$output'"
fi

test_case "_needle_dispatch_agent handles backticks in prompt"
result=$(_needle_dispatch_agent "test-echo" "$TEST_DIR" 'Run `ls` command' "nd-test" "Title" 0)
output_file=$(echo "$result" | cut -d'|' -f3)
output=$(cat "$output_file")
rm -f "$output_file"
if [[ "$output" == *'`ls`'* ]]; then
    test_pass
else
    test_fail "Expected backticks preserved, got '$output'"
fi

test_case "_needle_dispatch_agent handles newlines in prompt"
result=$(_needle_dispatch_agent "test-echo" "$TEST_DIR" $'Line 1\nLine 2\nLine 3' "nd-test" "Title" 0)
output_file=$(echo "$result" | cut -d'|' -f3)
output=$(cat "$output_file")
rm -f "$output_file"
if [[ "$output" == *"Line 1"* ]] && [[ "$output" == *"Line 2"* ]] && [[ "$output" == *"Line 3"* ]]; then
    test_pass
else
    test_fail "Expected newlines preserved, got '$output'"
fi

popd >/dev/null

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
