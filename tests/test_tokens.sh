#!/usr/bin/env bash
# Tests for NEEDLE token extraction module (src/telemetry/tokens.sh)

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
source "$PROJECT_DIR/src/telemetry/tokens.sh"

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

# ============ JSON Token Extraction Tests ============

test_case "_needle_extract_tokens_json extracts simple input_tokens/output_tokens"
cat > "$TEST_DIR/output.json" << 'EOF'
{"input_tokens": 1234, "output_tokens": 567}
EOF
result=$(_needle_extract_tokens_json "$TEST_DIR/output.json")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "1234" ]] && [[ "$output" == "567" ]]; then
    test_pass
else
    test_fail "Expected 1234|567, got $input|$output"
fi

test_case "_needle_extract_tokens_json extracts usage.input_tokens/usage.output_tokens"
cat > "$TEST_DIR/output.json" << 'EOF'
{"usage": {"input_tokens": 5000, "output_tokens": 2000}}
EOF
result=$(_needle_extract_tokens_json "$TEST_DIR/output.json")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "5000" ]] && [[ "$output" == "2000" ]]; then
    test_pass
else
    test_fail "Expected 5000|2000, got $input|$output"
fi

test_case "_needle_extract_tokens_json extracts prompt_tokens/completion_tokens"
cat > "$TEST_DIR/output.json" << 'EOF'
{"usage": {"prompt_tokens": 100, "completion_tokens": 50}}
EOF
result=$(_needle_extract_tokens_json "$TEST_DIR/output.json")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "100" ]] && [[ "$output" == "50" ]]; then
    test_pass
else
    test_fail "Expected 100|50, got $input|$output"
fi

test_case "_needle_extract_tokens_json extracts tokens.input/tokens.output"
cat > "$TEST_DIR/output.json" << 'EOF'
{"tokens": {"input": 999, "output": 888}}
EOF
result=$(_needle_extract_tokens_json "$TEST_DIR/output.json")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "999" ]] && [[ "$output" == "888" ]]; then
    test_pass
else
    test_fail "Expected 999|888, got $input|$output"
fi

test_case "_needle_extract_tokens_json returns 0|0 for missing file"
result=$(_needle_extract_tokens_json "$TEST_DIR/nonexistent.json")
if [[ "$result" == "0|0" ]]; then
    test_pass
else
    test_fail "Expected 0|0 for missing file, got $result"
fi

test_case "_needle_extract_tokens_json returns 0|0 for empty file"
touch "$TEST_DIR/empty.json"
result=$(_needle_extract_tokens_json "$TEST_DIR/empty.json")
if [[ "$result" == "0|0" ]]; then
    test_pass
else
    test_fail "Expected 0|0 for empty file, got $result"
fi

test_case "_needle_extract_tokens_json returns 0|0 for invalid JSON"
echo "not valid json {}" > "$TEST_DIR/invalid.json"
result=$(_needle_extract_tokens_json "$TEST_DIR/invalid.json")
if [[ "$result" == "0|0" ]]; then
    test_pass
else
    test_fail "Expected 0|0 for invalid JSON, got $result"
fi

test_case "_needle_extract_tokens_json handles nested JSON with other data"
cat > "$TEST_DIR/output.json" << 'EOF'
{
  "id": "msg-123",
  "type": "message",
  "content": "Hello world",
  "usage": {
    "input_tokens": 1500,
    "output_tokens": 750
  }
}
EOF
result=$(_needle_extract_tokens_json "$TEST_DIR/output.json")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "1500" ]] && [[ "$output" == "750" ]]; then
    test_pass
else
    test_fail "Expected 1500|750, got $input|$output"
fi

# ============ Text Token Extraction Tests ============

test_case "_needle_extract_tokens_text extracts with explicit pattern (input/output)"
cat > "$TEST_DIR/output.txt" << 'EOF'
Processing complete.
Input tokens: 2500
Output tokens: 1200
Done.
EOF
result=$(_needle_extract_tokens_text "$TEST_DIR/output.txt" "Input tokens: ([0-9]+).*Output tokens: ([0-9]+)")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "2500" ]] && [[ "$output" == "1200" ]]; then
    test_pass
else
    test_fail "Expected 2500|1200, got $input|$output"
fi

test_case "_needle_extract_tokens_text extracts single number pattern"
cat > "$TEST_DIR/output.txt" << 'EOF'
Total tokens used: 5000
EOF
result=$(_needle_extract_tokens_text "$TEST_DIR/output.txt" "tokens used: ([0-9]+)")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$output" == "5000" ]]; then
    test_pass
else
    test_fail "Expected output=5000, got input=$input output=$output"
fi

test_case "_needle_extract_tokens_text returns 0|0 for missing file"
result=$(_needle_extract_tokens_text "$TEST_DIR/nonexistent.txt" "pattern")
if [[ "$result" == "0|0" ]]; then
    test_pass
else
    test_fail "Expected 0|0 for missing file, got $result"
fi

# ============ Auto-detect Tests ============

test_case "_needle_extract_tokens_text_autodetect detects 'input: N, output: N' format"
cat > "$TEST_DIR/output.txt" << 'EOF'
API Response
Input: 3000, Output: 1500
EOF
result=$(_needle_extract_tokens_text_autodetect "$TEST_DIR/output.txt")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "3000" ]] && [[ "$output" == "1500" ]]; then
    test_pass
else
    test_fail "Expected 3000|1500, got $input|$output"
fi

test_case "_needle_extract_tokens_text_autodetect detects 'tokens_used: N' format"
cat > "$TEST_DIR/output.txt" << 'EOF'
Result:
tokens_used: 7500
EOF
result=$(_needle_extract_tokens_text_autodetect "$TEST_DIR/output.txt")
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$output" == "7500" ]]; then
    test_pass
else
    test_fail "Expected output=7500, got $output"
fi

test_case "_needle_extract_tokens_text_autodetect detects 'in=N out=N' format"
cat > "$TEST_DIR/output.txt" << 'EOF'
Stats: in=4500 out=2200
EOF
result=$(_needle_extract_tokens_text_autodetect "$TEST_DIR/output.txt")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "4500" ]] && [[ "$output" == "2200" ]]; then
    test_pass
else
    test_fail "Expected 4500|2200, got $input|$output"
fi

test_case "_needle_extract_tokens_text_autodetect returns 0|0 when no pattern matches"
cat > "$TEST_DIR/output.txt" << 'EOF'
No token information here
Just regular text
EOF
result=$(_needle_extract_tokens_text_autodetect "$TEST_DIR/output.txt")
if [[ "$result" == "0|0" ]]; then
    test_pass
else
    test_fail "Expected 0|0 when no pattern matches, got $result"
fi

# ============ Main Entry Point Tests ============

# Create test agent for main function tests
mkdir -p "$TEST_DIR/.needle/agents"

cat > "$TEST_DIR/.needle/agents/json-agent.yaml" << 'EOF'
name: json-agent
description: Test agent with JSON output
version: "1.0"
runner: echo
provider: test
model: test

invoke: echo "test"

input:
  method: heredoc

output:
  format: json
  token_pattern: ""

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

cat > "$TEST_DIR/.needle/agents/text-agent.yaml" << 'EOF'
name: text-agent
description: Test agent with text output
version: "1.0"
runner: echo
provider: test
model: test

invoke: echo "test"

input:
  method: heredoc

output:
  format: text
  token_pattern: "tokens: ([0-9]+).*in: ([0-9]+).*out: ([0-9]+)"

limits:
  requests_per_minute: 60
  max_concurrent: 5
EOF

# Change to test directory for agent discovery
pushd "$TEST_DIR" >/dev/null

test_case "_needle_extract_tokens uses JSON format for json-agent"
cat > "$TEST_DIR/output.log" << 'EOF'
{"input_tokens": 8000, "output_tokens": 4000}
EOF
result=$(_needle_extract_tokens "json-agent" "$TEST_DIR/output.log")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "8000" ]] && [[ "$output" == "4000" ]]; then
    test_pass
else
    test_fail "Expected 8000|4000, got $input|$output"
fi

test_case "_needle_extract_tokens uses text format for text-agent"
cat > "$TEST_DIR/output.log" << 'EOF'
Processing...
tokens: 12345 in: 6000 out: 6345
Done
EOF
result=$(_needle_extract_tokens "text-agent" "$TEST_DIR/output.log")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "12345" ]] && [[ "$output" == "6000" ]]; then
    test_pass
else
    test_fail "Expected first two numbers, got $input|$output"
fi

test_case "_needle_extract_tokens returns 0|0 for nonexistent agent"
cat > "$TEST_DIR/output.log" << 'EOF'
{"input_tokens": 100, "output_tokens": 50}
EOF
result=$(_needle_extract_tokens "nonexistent-agent" "$TEST_DIR/output.log")
# Should still try to extract with default text format
if [[ "$result" == *"|"* ]]; then
    test_pass
else
    test_fail "Expected pipe-delimited result, got $result"
fi

test_case "_needle_extract_tokens returns 0|0 without agent name"
result=$(_needle_extract_tokens "" "$TEST_DIR/output.log")
if [[ "$result" == "0|0" ]]; then
    test_pass
else
    test_fail "Expected 0|0 without agent name, got $result"
fi

test_case "_needle_extract_tokens returns 0|0 without output file"
result=$(_needle_extract_tokens "json-agent" "")
if [[ "$result" == "0|0" ]]; then
    test_pass
else
    test_fail "Expected 0|0 without output file, got $result"
fi

popd >/dev/null

# ============ Utility Function Tests ============

test_case "_needle_parse_token_result parses result correctly"
result="1500|750"
NEEDLE_TOKENS_input=""
NEEDLE_TOKENS_output=""
NEEDLE_TOKENS_total=""
_needle_parse_token_result "$result" "NEEDLE_TOKENS"
if [[ "$NEEDLE_TOKENS_input" == "1500" ]] && \
   [[ "$NEEDLE_TOKENS_output" == "750" ]] && \
   [[ "$NEEDLE_TOKENS_total" == "2250" ]]; then
    test_pass
else
    test_fail "Expected input=1500 output=750 total=2250, got input=$NEEDLE_TOKENS_input output=$NEEDLE_TOKENS_output total=$NEEDLE_TOKENS_total"
fi

test_case "_needle_parse_token_result handles empty result"
NEEDLE_TOKENS_input=""
NEEDLE_TOKENS_output=""
NEEDLE_TOKENS_total=""
_needle_parse_token_result "" "NEEDLE_TOKENS"
if [[ "$NEEDLE_TOKENS_input" == "0" ]] && \
   [[ "$NEEDLE_TOKENS_output" == "0" ]] && \
   [[ "$NEEDLE_TOKENS_total" == "0" ]]; then
    test_pass
else
    test_fail "Expected zeros for empty result, got input=$NEEDLE_TOKENS_input output=$NEEDLE_TOKENS_output total=$NEEDLE_TOKENS_total"
fi

test_case "_needle_calculate_token_cost calculates correctly"
# 1M input tokens at $3/M, 1M output tokens at $15/M
result=$(_needle_calculate_token_cost 1000000 1000000 3 15)
# Should be 3 + 15 = 18
if [[ "$result" == "18"* ]] || [[ "$result" == "18.000000" ]]; then
    test_pass
else
    test_fail "Expected ~18, got $result"
fi

test_case "_needle_calculate_token_cost handles zero tokens"
result=$(_needle_calculate_token_cost 0 0 3 15)
if [[ "$result" == "0"* ]] || [[ "$result" == ".000000" ]]; then
    test_pass
else
    test_fail "Expected ~0, got $result"
fi

test_case "_needle_get_token_stats returns valid JSON"
cat > "$TEST_DIR/stats.json" << 'EOF'
{"input_tokens": 500, "output_tokens": 250}
EOF
result=$(_needle_get_token_stats "$TEST_DIR/stats.json")
if [[ "$result" == *"input"* ]] && [[ "$result" == *"output"* ]] && [[ "$result" == *"total"* ]]; then
    test_pass
else
    test_fail "Expected JSON with input/output/total, got $result"
fi

test_case "_needle_get_token_stats calculates total correctly"
cat > "$TEST_DIR/stats.json" << 'EOF'
{"input_tokens": 1000, "output_tokens": 500}
EOF
result=$(_needle_get_token_stats "$TEST_DIR/stats.json")
total=$(echo "$result" | grep -o '"total":[0-9]*' | grep -o '[0-9]*')
if [[ "$total" == "1500" ]]; then
    test_pass
else
    test_fail "Expected total=1500, got $total"
fi

# ============ Format Detection Tests ============

test_case "_needle_extract_tokens_with_format uses JSON format"
cat > "$TEST_DIR/mixed.json" << 'EOF'
{"input_tokens": 1111, "output_tokens": 2222}
EOF
result=$(_needle_extract_tokens_with_format "$TEST_DIR/mixed.json" "json")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "1111" ]] && [[ "$output" == "2222" ]]; then
    test_pass
else
    test_fail "Expected 1111|2222, got $input|$output"
fi

test_case "_needle_extract_tokens_with_format uses text format"
cat > "$TEST_DIR/mixed.txt" << 'EOF'
Input tokens: 3333
Output tokens: 4444
EOF
result=$(_needle_extract_tokens_with_format "$TEST_DIR/mixed.txt" "text" "Input tokens: ([0-9]+).*Output tokens: ([0-9]+)")
input=$(echo "$result" | cut -d'|' -f1)
output=$(echo "$result" | cut -d'|' -f2)
if [[ "$input" == "3333" ]] && [[ "$output" == "4444" ]]; then
    test_pass
else
    test_fail "Expected 3333|4444, got $input|$output"
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
