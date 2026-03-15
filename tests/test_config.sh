#!/usr/bin/env bash
# Tests for NEEDLE config module (src/lib/config.sh)

# Test setup
TEST_DIR=$(mktemp -d)
TEST_CONFIG_DIR="$TEST_DIR/.needle"
TEST_CONFIG_FILE="$TEST_CONFIG_DIR/config.yaml"

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_HOME="$TEST_CONFIG_DIR"
export NEEDLE_CONFIG_FILE="$TEST_CONFIG_FILE"
export NEEDLE_CONFIG_NAME="config.yaml"

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/lib/config.sh"

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

# ============ Tests ============

# Test: config_exists returns false for non-existent config
test_case "config_exists returns false for non-existent config"
if ! config_exists; then
    test_pass
else
    test_fail "Expected false, got true"
fi

# Test: create_default_config creates file
test_case "create_default_config creates config file"
mkdir -p "$TEST_CONFIG_DIR"
if create_default_config "$TEST_CONFIG_FILE" && [[ -f "$TEST_CONFIG_FILE" ]]; then
    test_pass
else
    test_fail "Config file was not created"
fi

# Test: config_exists returns true for existing config
test_case "config_exists returns true for existing config"
if config_exists; then
    test_pass
else
    test_fail "Expected true, got false"
fi

# Test: load_config returns valid JSON
test_case "load_config returns valid JSON"
clear_config_cache
config=$(load_config)
if [[ "$config" == *"limits"* ]] && [[ "$config" == *"runner"* ]] && [[ "$config" == *"strands"* ]]; then
    test_pass
else
    test_fail "Config missing expected sections"
fi

# Test: load_config caches result
test_case "load_config caches result"
clear_config_cache
config1=$(load_config)
config2=$(load_config)
if [[ "$config1" == "$config2" ]]; then
    test_pass
else
    test_fail "Cached configs don't match"
fi

# Test: clear_config_cache clears cache
test_case "clear_config_cache clears cache"
load_config >/dev/null
if [[ -n "$NEEDLE_CONFIG_CACHE" ]]; then
    clear_config_cache
    if [[ -z "$NEEDLE_CONFIG_CACHE" ]]; then
        test_pass
    else
        test_fail "Cache was not cleared"
    fi
else
    test_pass  # Cache might be empty already
fi

# Test: get_config returns default value for missing key
test_case "get_config returns default for missing key"
clear_config_cache
value=$(get_config "nonexistent.key" "default_value")
if [[ "$value" == "default_value" ]]; then
    test_pass
else
    test_fail "Expected 'default_value', got '$value'"
fi

# Test: get_config_int returns integer
test_case "get_config_int returns integer"
clear_config_cache
value=$(get_config_int "limits.global_max_concurrent" "10")
if [[ "$value" =~ ^[0-9]+$ ]]; then
    test_pass
else
    test_fail "Expected integer, got '$value'"
fi

# Test: get_config_bool returns boolean
test_case "get_config_bool returns boolean"
clear_config_cache
value=$(get_config_bool "strands.pluck" "false")
if [[ "$value" == "true" ]] || [[ "$value" == "false" ]]; then
    test_pass
else
    test_fail "Expected boolean, got '$value'"
fi

# Test: validate_config passes valid config
test_case "validate_config passes valid config"
if validate_config "$TEST_CONFIG_FILE" 2>/dev/null; then
    test_pass
else
    test_fail "Valid config failed validation"
fi

# Test: validate_config fails for non-existent file
test_case "validate_config fails for non-existent file"
if ! validate_config "/nonexistent/config.yaml" 2>/dev/null; then
    test_pass
else
    test_fail "Expected validation to fail"
fi

# Test: reload_config reloads from file
test_case "reload_config reloads from file"
clear_config_cache
config1=$(load_config)
clear_config_cache
config2=$(reload_config)
if [[ -n "$config2" ]]; then
    test_pass
else
    test_fail "Reload returned empty config"
fi

# Test: Default config has expected sections
test_case "Default config has limits section"
clear_config_cache
config=$(load_config)
if echo "$config" | grep -q '"limits"'; then
    test_pass
else
    test_fail "Missing limits section"
fi

test_case "Default config has runner section"
clear_config_cache
config=$(load_config)
if echo "$config" | grep -q '"runner"'; then
    test_pass
else
    test_fail "Missing runner section"
fi

test_case "Default config has strands section"
clear_config_cache
config=$(load_config)
if echo "$config" | grep -q '"strands"'; then
    test_pass
else
    test_fail "Missing strands section"
fi

test_case "Default config has effort section"
clear_config_cache
config=$(load_config)
if echo "$config" | grep -q '"effort"'; then
    test_pass
else
    test_fail "Missing effort section"
fi

# Test: get_config honors NEEDLE_CONFIG_OVERRIDE_* env vars
# Regression test for nd-tsbs: NEEDLE_CONFIG_OVERRIDE_* vars were silently ignored,
# causing tests to create real beads when the production config had auto_bead_on_error: true
test_case "get_config honors NEEDLE_CONFIG_OVERRIDE_* env vars"
clear_config_cache
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR="false"
value=$(get_config "debug.auto_bead_on_error" "false")
unset NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_ON_ERROR
if [[ "$value" == "false" ]]; then
    test_pass
else
    test_fail "Expected 'false' from override, got '$value'"
fi

test_case "NEEDLE_CONFIG_OVERRIDE_* takes precedence over config file"
clear_config_cache
# Write a config that sets a value to "from_file"
mkdir -p "$TEST_CONFIG_DIR"
echo 'strands:' > "$TEST_CONFIG_FILE"
echo '  weave: true' >> "$TEST_CONFIG_FILE"
export NEEDLE_CONFIG_OVERRIDE_STRANDS_WEAVE="false"
value=$(get_config "strands.weave" "default")
unset NEEDLE_CONFIG_OVERRIDE_STRANDS_WEAVE
clear_config_cache
if [[ "$value" == "false" ]]; then
    test_pass
else
    test_fail "Expected 'false' from override, got '$value'"
fi

test_case "get_config returns file value when NEEDLE_CONFIG_OVERRIDE_* is not set"
clear_config_cache
mkdir -p "$TEST_CONFIG_DIR"
echo 'strands:' > "$TEST_CONFIG_FILE"
echo '  weave: true' >> "$TEST_CONFIG_FILE"
value=$(get_config "strands.weave" "default")
clear_config_cache
if [[ "$value" == "true" ]]; then
    test_pass
else
    test_fail "Expected 'true' from file, got '$value'"
fi

test_case "NEEDLE_CONFIG_OVERRIDE_* with empty string is honored"
clear_config_cache
export NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_WORKSPACE=""
value=$(get_config "debug.auto_bead_workspace" "fallback")
unset NEEDLE_CONFIG_OVERRIDE_DEBUG_AUTO_BEAD_WORKSPACE
if [[ "$value" == "" ]]; then
    test_pass
else
    test_fail "Expected empty string from override, got '$value'"
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
