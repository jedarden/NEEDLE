#!/usr/bin/env bash
# Tests for NEEDLE worker naming module (src/runner/naming.sh)

# Test setup - create temp directory
TEST_DIR=$(mktemp -d)
TEST_NEEDLE_HOME="$TEST_DIR/.needle"

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_HOME="$TEST_NEEDLE_HOME"
export NEEDLE_STATE_DIR="state"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=true

# Source required modules (constants.sh MUST come before naming.sh)
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/runner/naming.sh"

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

# ============ validate_identifier Tests ============

# Test: valid identifier - simple lowercase
test_case "validate_identifier accepts simple lowercase"
if validate_identifier "alpha"; then
    test_pass
else
    test_fail "Should accept 'alpha'"
fi

# Test: valid identifier - with numbers
test_case "validate_identifier accepts lowercase with numbers"
if validate_identifier "alpha123"; then
    test_pass
else
    test_fail "Should accept 'alpha123'"
fi

# Test: valid identifier - with hyphens
test_case "validate_identifier accepts lowercase with hyphens"
if validate_identifier "alpha-one"; then
    test_pass
else
    test_fail "Should accept 'alpha-one'"
fi

# Test: valid identifier - complex
test_case "validate_identifier accepts complex identifier"
if validate_identifier "worker-1-beta"; then
    test_pass
else
    test_fail "Should accept 'worker-1-beta'"
fi

# Test: invalid identifier - empty
test_case "validate_identifier rejects empty string"
if ! validate_identifier ""; then
    test_pass
else
    test_fail "Should reject empty string"
fi

# Test: invalid identifier - starts with number
test_case "validate_identifier rejects starting with number"
if ! validate_identifier "1alpha"; then
    test_pass
else
    test_fail "Should reject '1alpha'"
fi

# Test: invalid identifier - uppercase
test_case "validate_identifier rejects uppercase"
if ! validate_identifier "Alpha"; then
    test_pass
else
    test_fail "Should reject 'Alpha'"
fi

# Test: invalid identifier - special chars
test_case "validate_identifier rejects special characters"
if ! validate_identifier "alpha_one"; then
    test_pass
else
    test_fail "Should reject 'alpha_one'"
fi

# Test: invalid identifier - spaces
test_case "validate_identifier rejects spaces"
if ! validate_identifier "alpha one"; then
    test_pass
else
    test_fail "Should reject 'alpha one'"
fi

# ============ validate_identifier_verbose Tests ============

# Test: verbose validation - valid
test_case "validate_identifier_verbose returns 0 for valid"
output=$(validate_identifier_verbose "valid-id" 2>&1)
if [[ $? -eq 0 ]] && [[ -z "$output" ]]; then
    test_pass
else
    test_fail "Should return 0 with no output for valid id"
fi

# Test: verbose validation - invalid
test_case "validate_identifier_verbose returns 1 with error for invalid"
output=$(validate_identifier_verbose "Invalid" 2>&1)
if [[ $? -eq 1 ]] && [[ -n "$output" ]]; then
    test_pass
else
    test_fail "Should return 1 with error message for invalid id"
fi

# ============ get_next_identifier_from_list Tests ============

# Test: first available with empty list
test_case "get_next_identifier_from_list returns alpha for empty list"
result=$(get_next_identifier_from_list "")
if [[ "$result" == "alpha" ]]; then
    test_pass
else
    test_fail "Expected 'alpha', got '$result'"
fi

# Test: skip used identifiers
test_case "get_next_identifier_from_list skips alpha when used"
result=$(get_next_identifier_from_list "alpha")
if [[ "$result" == "bravo" ]]; then
    test_pass
else
    test_fail "Expected 'bravo', got '$result'"
fi

# Test: skip multiple used identifiers
test_case "get_next_identifier_from_list skips alpha, bravo, charlie"
result=$(get_next_identifier_from_list "alpha bravo charlie")
if [[ "$result" == "delta" ]]; then
    test_pass
else
    test_fail "Expected 'delta', got '$result'"
fi

# Test: skip many used identifiers
test_case "get_next_identifier_from_list skips first 10"
result=$(get_next_identifier_from_list "alpha bravo charlie delta echo foxtrot golf hotel india juliet")
if [[ "$result" == "kilo" ]]; then
    test_pass
else
    test_fail "Expected 'kilo', got '$result'"
fi

# Test: handle all 26 used - fallback
test_case "get_next_identifier_from_list handles exhaustion with numeric suffix"
all_nato="${NEEDLE_NATO_ALPHABET[*]}"
result=$(get_next_identifier_from_list "$all_nato")
if [[ "$result" == "alpha-27" ]]; then
    test_pass
else
    test_fail "Expected 'alpha-27', got '$result'"
fi

# Test: handle 26 used with newline-separated list
test_case "get_next_identifier_from_list handles newline-separated list"
all_nato_newline=$(printf '%s\n' "${NEEDLE_NATO_ALPHABET[@]}")
result=$(get_next_identifier_from_list "$all_nato_newline")
if [[ "$result" == "alpha-27" ]]; then
    test_pass
else
    test_fail "Expected 'alpha-27' with newline list, got '$result'"
fi

# ============ is_nato_identifier Tests ============

# Test: is_nato_identifier - true for valid
test_case "is_nato_identifier returns true for 'alpha'"
if is_nato_identifier "alpha"; then
    test_pass
else
    test_fail "Should return true for 'alpha'"
fi

# Test: is_nato_identifier - true for 'zulu'
test_case "is_nato_identifier returns true for 'zulu'"
if is_nato_identifier "zulu"; then
    test_pass
else
    test_fail "Should return true for 'zulu'"
fi

# Test: is_nato_identifier - false for non-nato
test_case "is_nato_identifier returns false for 'custom'"
if ! is_nato_identifier "custom"; then
    test_pass
else
    test_fail "Should return false for 'custom'"
fi

# Test: is_nato_identifier - false for numeric suffix
test_case "is_nato_identifier returns false for 'alpha-1'"
if ! is_nato_identifier "alpha-1"; then
    test_pass
else
    test_fail "Should return false for 'alpha-1'"
fi

# ============ get_nato_index Tests ============

# Test: get_nato_index - alpha is 0
test_case "get_nato_index returns 0 for 'alpha'"
result=$(get_nato_index "alpha")
if [[ "$result" == "0" ]]; then
    test_pass
else
    test_fail "Expected 0, got '$result'"
fi

# Test: get_nato_index - charlie is 2
test_case "get_nato_index returns 2 for 'charlie'"
result=$(get_nato_index "charlie")
if [[ "$result" == "2" ]]; then
    test_pass
else
    test_fail "Expected 2, got '$result'"
fi

# Test: get_nato_index - zulu is 25
test_case "get_nato_index returns 25 for 'zulu'"
result=$(get_nato_index "zulu")
if [[ "$result" == "25" ]]; then
    test_pass
else
    test_fail "Expected 25, got '$result'"
fi

# Test: get_nato_index - invalid returns -1
test_case "get_nato_index returns -1 for invalid"
result=$(get_nato_index "invalid")
if [[ "$result" == "-1" ]]; then
    test_pass
else
    test_fail "Expected -1, got '$result'"
fi

# ============ get_nato_by_index Tests ============

# Test: get_nato_by_index - 0 returns alpha
test_case "get_nato_by_index returns 'alpha' for 0"
result=$(get_nato_by_index 0)
if [[ "$result" == "alpha" ]]; then
    test_pass
else
    test_fail "Expected 'alpha', got '$result'"
fi

# Test: get_nato_by_index - 25 returns zulu
test_case "get_nato_by_index returns 'zulu' for 25"
result=$(get_nato_by_index 25)
if [[ "$result" == "zulu" ]]; then
    test_pass
else
    test_fail "Expected 'zulu', got '$result'"
fi

# Test: get_nato_by_index - negative fails
test_case "get_nato_by_index fails for negative"
if ! get_nato_by_index -1 2>/dev/null; then
    test_pass
else
    test_fail "Should fail for negative index"
fi

# Test: get_nato_by_index - out of range fails
test_case "get_nato_by_index fails for 26"
if ! get_nato_by_index 26 2>/dev/null; then
    test_pass
else
    test_fail "Should fail for index 26"
fi

# ============ parse_custom_id Tests ============

# Test: parse_custom_id - extract with space
test_case "parse_custom_id extracts '--id custom-1'"
result=$(parse_custom_id "--workspace" "/path" "--id" "custom-1")
if [[ "$result" == "custom-1" ]]; then
    test_pass
else
    test_fail "Expected 'custom-1', got '$result'"
fi

# Test: parse_custom_id - extract with equals
test_case "parse_custom_id extracts '--id=custom-2'"
result=$(parse_custom_id "--workspace" "/path" "--id=custom-2")
if [[ "$result" == "custom-2" ]]; then
    test_pass
else
    test_fail "Expected 'custom-2', got '$result'"
fi

# Test: parse_custom_id - no id returns empty
test_case "parse_custom_id returns empty when no --id"
result=$(parse_custom_id "--workspace" "/path" "--verbose")
if [[ -z "$result" ]]; then
    test_pass
else
    test_fail "Expected empty, got '$result'"
fi

# Test: parse_custom_id - handles --id at end
test_case "parse_custom_id handles --id at end without value"
result=$(parse_custom_id "--workspace" "/path" "--id")
if [[ -z "$result" ]]; then
    test_pass
else
    test_fail "Expected empty when --id has no value, got '$result'"
fi

# ============ NEEDLE_NATO_ALPHABET Array Tests ============

# Test: NATO array has 26 elements
test_case "NEEDLE_NATO_ALPHABET has 26 elements"
if [[ ${#NEEDLE_NATO_ALPHABET[@]} -eq 26 ]]; then
    test_pass
else
    test_fail "Expected 26, got ${#NEEDLE_NATO_ALPHABET[@]}"
fi

# Test: NATO array starts with alpha
test_case "NEEDLE_NATO_ALPHABET starts with 'alpha'"
if [[ "${NEEDLE_NATO_ALPHABET[0]}" == "alpha" ]]; then
    test_pass
else
    test_fail "Expected 'alpha', got '${NEEDLE_NATO_ALPHABET[0]}'"
fi

# Test: NATO array ends with zulu
test_case "NEEDLE_NATO_ALPHABET ends with 'zulu'"
if [[ "${NEEDLE_NATO_ALPHABET[25]}" == "zulu" ]]; then
    test_pass
else
    test_fail "Expected 'zulu', got '${NEEDLE_NATO_ALPHABET[25]}'"
fi

# Test: NATO array contains all expected values
test_case "NEEDLE_NATO_ALPHABET contains all NATO phonetic alphabet"
expected=(alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu)
match=true
for i in "${!expected[@]}"; do
    if [[ "${NEEDLE_NATO_ALPHABET[$i]}" != "${expected[$i]}" ]]; then
        match=false
        break
    fi
done
if $match; then
    test_pass
else
    test_fail "NATO array does not match expected values"
fi

# ============ Summary ============
echo ""
echo "========================================"
echo "Test Summary"
echo "========================================"
echo "Total:  $TESTS_RUN"
echo "Passed: $TESTS_PASSED"
echo "Failed: $TESTS_FAILED"
echo "========================================"

# Exit with appropriate code
if [[ $TESTS_FAILED -gt 0 ]]; then
    exit 1
fi
exit 0
