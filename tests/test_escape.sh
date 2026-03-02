#!/usr/bin/env bash
# Tests for NEEDLE escape module (src/agent/escape.sh)

# Test setup
TEST_DIR=$(mktemp -d)

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/agent/escape.sh"

# Suppress output for tests
export NEEDLE_QUIET=true

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

# ============ Tests for escape_for_heredoc ============

test_case "escape_for_heredoc - preserves content exactly"
input="Test's \"quote\" \`backtick\` \$var"
result=$(echo "$input" | escape_for_heredoc)
if [[ "$result" == "$input" ]]; then
    test_pass
else
    test_fail "Expected '$input', got '$result'"
fi

test_case "escape_for_heredoc - handles multi-line content"
input=$'Line 1\nLine 2\nLine 3'
result=$(echo "$input" | escape_for_heredoc)
if [[ "$result" == "$input" ]]; then
    test_pass
else
    test_fail "Multi-line content not preserved"
fi

test_case "escape_for_heredoc - handles empty input"
result=$(echo "" | escape_for_heredoc)
if [[ -z "$result" ]]; then
    test_pass
else
    test_fail "Expected empty output"
fi

# ============ Tests for escape_for_single_quotes ============

test_case "escape_for_single_quotes - escapes single quote"
input="Test's quote"
expected="Test'\\''s quote"
result=$(echo "$input" | escape_for_single_quotes)
if [[ "$result" == "$expected" ]]; then
    test_pass
else
    test_fail "Expected '$expected', got '$result'"
fi

test_case "escape_for_single_quotes - handles multiple single quotes"
input="It's John's car's wheel"
expected="It'\\''s John'\\''s car'\\''s wheel"
result=$(echo "$input" | escape_for_single_quotes)
if [[ "$result" == "$expected" ]]; then
    test_pass
else
    test_fail "Expected '$expected', got '$result'"
fi

test_case "escape_for_single_quotes - preserves other special chars"
input='Test $var `cmd` "double"'
expected='Test $var `cmd` "double"'
result=$(echo "$input" | escape_for_single_quotes)
if [[ "$result" == "$expected" ]]; then
    test_pass
else
    test_fail "Expected '$expected', got '$result'"
fi

test_case "escape_for_single_quotes - handles leading/trailing quotes"
input="'quoted'"
# Input 'quoted' -> '\''quoted'\''
# Leading ' becomes '\''  then quoted then trailing ' becomes '\''
result=$(echo "$input" | escape_for_single_quotes)
if [[ "$result" == "'\\''quoted'\\''" ]]; then
    test_pass
else
    test_fail "Expected '\\'\\''quoted'\\''', got '$result'"
fi

# ============ Tests for escape_for_double_quotes ============

test_case "escape_for_double_quotes - escapes dollar sign"
input='Test $var'
expected='Test \$var'
result=$(echo "$input" | escape_for_double_quotes)
if [[ "$result" == "$expected" ]]; then
    test_pass
else
    test_fail "Expected '$expected', got '$result'"
fi

test_case "escape_for_double_quotes - escapes backtick"
input='Test `cmd`'
expected='Test \`cmd\`'
result=$(echo "$input" | escape_for_double_quotes)
if [[ "$result" == "$expected" ]]; then
    test_pass
else
    test_fail "Expected '$expected', got '$result'"
fi

test_case "escape_for_double_quotes - escapes double quote"
input='Test "quoted"'
expected='Test \"quoted\"'
result=$(echo "$input" | escape_for_double_quotes)
if [[ "$result" == "$expected" ]]; then
    test_pass
else
    test_fail "Expected '$expected', got '$result'"
fi

test_case "escape_for_double_quotes - escapes backslash"
input='Test \path'
expected='Test \\path'
result=$(echo "$input" | escape_for_double_quotes)
if [[ "$result" == "$expected" ]]; then
    test_pass
else
    test_fail "Expected '$expected', got '$result'"
fi

test_case "escape_for_double_quotes - handles complex string"
input='Test $var `cmd` "quote" \slash'
result=$(echo "$input" | escape_for_double_quotes)
# Verify all special chars are escaped (check for backslash before each)
if [[ "$result" == *'\$'* ]] && [[ "$result" == *'\`'* ]] && [[ "$result" == *'\"'* ]] && [[ "$result" == *'\\'* ]]; then
    test_pass
else
    test_fail "Not all special chars escaped: '$result'"
fi

# ============ Tests for escape_for_json ============

test_case "escape_for_json - escapes double quotes"
input='Test "quoted"'
expected='Test \"quoted\"'
result=$(echo "$input" | escape_for_json)
if [[ "$result" == "$expected" ]]; then
    test_pass
else
    test_fail "Expected '$expected', got '$result'"
fi

test_case "escape_for_json - escapes backslashes"
input='Test \path'
expected='Test \\path'
result=$(echo "$input" | escape_for_json)
if [[ "$result" == "$expected" ]]; then
    test_pass
else
    test_fail "Expected '$expected', got '$result'"
fi

test_case "escape_for_json - converts newlines to \\n"
input=$'Line 1\nLine 2'
result=$(echo "$input" | escape_for_json)
# Output should have literal \n (backslash followed by n)
if [[ "$result" == "Line 1\\nLine 2" ]]; then
    test_pass
else
    test_fail "Expected 'Line 1\\nLine 2', got '$result'"
fi

# ============ Tests for escape_backticks ============

test_case "escape_backticks - escapes backticks"
input='Test `cmd`'
expected='Test \`cmd\`'
result=$(echo "$input" | escape_backticks)
if [[ "$result" == "$expected" ]]; then
    test_pass
else
    test_fail "Expected '$expected', got '$result'"
fi

test_case "escape_backticks - handles multiple backticks"
input='``double``'
expected='\`\`double\`\`'
result=$(echo "$input" | escape_backticks)
if [[ "$result" == "$expected" ]]; then
    test_pass
else
    test_fail "Expected '$expected', got '$result'"
fi

# ============ Tests for escape_dollar_signs ============

test_case "escape_dollar_signs - escapes dollar signs"
input='Test $var ${var}'
expected='Test \$var \${var}'
result=$(echo "$input" | escape_dollar_signs)
if [[ "$result" == "$expected" ]]; then
    test_pass
else
    test_fail "Expected '$expected', got '$result'"
fi

# ============ Tests for contains_dangerous_chars ============

test_case "contains_dangerous_chars - detects single quote"
input="Test's"
if contains_dangerous_chars "$input"; then
    test_pass
else
    test_fail "Should detect single quote as dangerous"
fi

test_case "contains_dangerous_chars - detects dollar sign"
input='Test $var'
if contains_dangerous_chars "$input"; then
    test_pass
else
    test_fail "Should detect dollar sign as dangerous"
fi

test_case "contains_dangerous_chars - detects backtick"
input='Test `cmd`'
if contains_dangerous_chars "$input"; then
    test_pass
else
    test_fail "Should detect backtick as dangerous"
fi

test_case "contains_dangerous_chars - returns false for safe string"
input='Simple text with no special chars'
if ! contains_dangerous_chars "$input"; then
    test_pass
else
    test_fail "Should not flag safe string as dangerous"
fi

# ============ Tests for escape_prompt (main function) ============

test_case "escape_prompt - heredoc method preserves content"
input="Test's \"quote\" \`backtick\` \$var"
result=$(echo "$input" | escape_prompt heredoc)
if [[ "$result" == "$input" ]]; then
    test_pass
else
    test_fail "Heredoc method should preserve content"
fi

test_case "escape_prompt - single_quotes method escapes quotes"
input="Test's quote"
result=$(echo "$input" | escape_prompt single_quotes)
if [[ "$result" == *"\\''"* ]]; then
    test_pass
else
    test_fail "Single quotes method should escape quotes"
fi

test_case "escape_prompt - sq alias works"
input="Test's quote"
result=$(echo "$input" | escape_prompt sq)
if [[ "$result" == *"\\''"* ]]; then
    test_pass
else
    test_fail "sq alias should work like single_quotes"
fi

test_case "escape_prompt - double_quotes method escapes special chars"
input='Test $var'
result=$(echo "$input" | escape_prompt double_quotes)
if [[ "$result" == *'\$'* ]]; then
    test_pass
else
    test_fail "Double quotes method should escape $"
fi

test_case "escape_prompt - default is single_quotes"
input="Test's"
result=$(echo "$input" | escape_prompt)
if [[ "$result" == *"\\''"* ]]; then
    test_pass
else
    test_fail "Default should be single_quotes"
fi

test_case "escape_prompt - unknown method passes through with warning"
input="Test input"
result=$(echo "$input" | escape_prompt unknown_method 2>&1)
if [[ "$result" == *"Test input"* ]] && [[ "$result" == *"Warning"* ]]; then
    test_pass
else
    test_fail "Unknown method should pass through with warning"
fi

test_case "escape_prompt - raw method passes through"
input="Test's \"quote\""
result=$(echo "$input" | escape_prompt raw)
if [[ "$result" == "$input" ]]; then
    test_pass
else
    test_fail "raw method should pass through"
fi

# ============ Tests for multi-line handling ============

test_case "escape_prompt - handles multi-line with heredoc"
input=$'Line 1\nLine 2\nLine 3'
result=$(echo "$input" | escape_prompt heredoc)
if [[ "$result" == "$input" ]]; then
    test_pass
else
    test_fail "Should handle multi-line with heredoc"
fi

test_case "escape_prompt - handles multi-line with single_quotes"
input=$'Line 1\nLine\'s 2'
result=$(echo "$input" | escape_prompt single_quotes)
if [[ "$result" == *"\\''"* ]] && [[ "$result" == *"Line 1"* ]]; then
    test_pass
else
    test_fail "Should handle multi-line with single_quotes"
fi

# ============ Tests for get_escape_method ============

test_case "get_escape_method - returns correct name for sq"
result=$(get_escape_method "sq")
if [[ "$result" == "single_quotes" ]]; then
    test_pass
else
    test_fail "Expected 'single_quotes', got '$result'"
fi

test_case "get_escape_method - returns correct name for dq"
result=$(get_escape_method "dq")
if [[ "$result" == "double_quotes" ]]; then
    test_pass
else
    test_fail "Expected 'double_quotes', got '$result'"
fi

test_case "get_escape_method - returns unknown for invalid alias"
result=$(get_escape_method "invalid")
if [[ "$result" == "unknown" ]]; then
    test_pass
else
    test_fail "Expected 'unknown', got '$result'"
fi

# ============ Real-world use cases ============

test_case "Real-world: Prompt with code block"
input=$'Here is some code:\n```bash\necho "Hello"\n```'
result=$(echo "$input" | escape_prompt heredoc)
if [[ "$result" == "$input" ]]; then
    test_pass
else
    test_fail "Should handle code blocks in heredoc mode"
fi

test_case "Real-world: Prompt with variable references"
input='Please explain $HOME and ${PATH}'
result=$(echo "$input" | escape_prompt double_quotes)
if [[ "$result" == *'\$HOME'* ]] && [[ "$result" == *'\${PATH}'* ]]; then
    test_pass
else
    test_fail "Should escape variable references for double quotes"
fi

test_case "Real-world: Complex JSON-like content"
input='{"key": "value", "nested": {"key2": "value2"}}'
result=$(echo "$input" | escape_for_json)
# Should escape the double quotes
if [[ "$result" == *'\"'* ]]; then
    test_pass
else
    test_fail "Should escape quotes in JSON content"
fi

# ============ Edge cases ============

test_case "Edge case: Empty string"
result=$(echo "" | escape_prompt single_quotes)
if [[ -z "$result" ]]; then
    test_pass
else
    test_fail "Empty string should remain empty"
fi

test_case "Edge case: Only a single quote"
input="'"
# Single ' becomes '\''
result=$(echo "$input" | escape_prompt single_quotes)
if [[ "$result" == "'\\''" ]]; then
    test_pass
else
    test_fail "Expected '\\'\\'', got '$result'"
fi

test_case "Edge case: Only special characters"
input='$`"\\'
result=$(echo "$input" | escape_prompt double_quotes)
# All should be escaped
if [[ "$result" == *'\$'* ]] && [[ "$result" == *'\`'* ]] && [[ "$result" == *'\"'* ]] && [[ "$result" == *'\\'* ]]; then
    test_pass
else
    test_fail "Should escape all special chars: '$result'"
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
