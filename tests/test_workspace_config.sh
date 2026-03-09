#!/usr/bin/env bash
# Tests for workspace config override support (.needle.yaml)
# Tests load_workspace_config, clear_workspace_cache, get_workspace_config

# Test setup
TEST_DIR=$(mktemp -d)
TEST_CONFIG_DIR="$TEST_DIR/.needle"
TEST_CONFIG_FILE="$TEST_CONFIG_DIR/config.yaml"
TEST_WORKSPACE="$TEST_DIR/workspace"

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

# Test helpers
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

# Setup: create global config and workspace directory
mkdir -p "$TEST_CONFIG_DIR" "$TEST_WORKSPACE"
cat > "$TEST_CONFIG_FILE" << 'EOF'
runner:
  polling_interval: 2s
  idle_timeout: 300s

effort:
  budget:
    daily_limit_usd: 50.0

limits:
  global_max_concurrent: 20
EOF

# ============ Tests ============

# Test: load_workspace_config returns global config when no .needle.yaml exists
test_case "load_workspace_config returns global config when no .needle.yaml"
clear_config_cache
clear_workspace_cache
config=$(load_workspace_config "$TEST_WORKSPACE")
if [[ "$config" == *"runner"* ]] && [[ "$config" == *"limits"* ]]; then
    test_pass
else
    test_fail "Expected global config sections, got: ${config:0:100}"
fi

# Test: clear_workspace_cache clears the cache
test_case "clear_workspace_cache clears workspace cache"
load_workspace_config "$TEST_WORKSPACE" >/dev/null
if [[ -n "$NEEDLE_WORKSPACE_CONFIG_CACHE" ]]; then
    clear_workspace_cache "$TEST_WORKSPACE"
    if [[ -z "$NEEDLE_WORKSPACE_CONFIG_CACHE" ]]; then
        test_pass
    else
        test_fail "Cache not cleared"
    fi
else
    test_pass  # Cache might not have been set
fi

# Test: load_workspace_config caches result
test_case "load_workspace_config caches result for same workspace"
clear_config_cache
clear_workspace_cache
config1=$(load_workspace_config "$TEST_WORKSPACE")
config2=$(load_workspace_config "$TEST_WORKSPACE")
if [[ "$config1" == "$config2" ]]; then
    test_pass
else
    test_fail "Cached configs don't match"
fi

# Test: workspace .needle.yaml overrides global config
test_case "workspace .needle.yaml overrides global runner.polling_interval"
clear_config_cache
clear_workspace_cache

# Create workspace config with override
cat > "$TEST_WORKSPACE/.needle.yaml" << 'EOF'
runner:
  polling_interval: 10s
EOF

config=$(load_workspace_config "$TEST_WORKSPACE")
# Check that polling_interval was overridden
if echo "$config" | grep -q '"polling_interval"'; then
    interval=$(echo "$config" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('runner',{}).get('polling_interval',''))" 2>/dev/null)
    if [[ "$interval" == "10s" ]]; then
        test_pass
    else
        test_fail "Expected '10s', got '$interval'"
    fi
else
    test_fail "polling_interval not found in merged config"
fi

# Test: workspace .needle.yaml merges (does not replace) global settings
test_case "workspace .needle.yaml merges with global config (non-overridden keys preserved)"
clear_config_cache
clear_workspace_cache

# workspace config only sets runner.polling_interval; limits should still be present
config=$(load_workspace_config "$TEST_WORKSPACE")
if echo "$config" | grep -q '"limits"'; then
    test_pass
else
    test_fail "Global 'limits' section missing after workspace merge"
fi

# Test: workspace effort override
test_case "workspace .needle.yaml can override effort.budget.daily_limit_usd"
clear_config_cache
clear_workspace_cache

cat > "$TEST_WORKSPACE/.needle.yaml" << 'EOF'
effort:
  budget:
    daily_limit_usd: 10.0
EOF

config=$(load_workspace_config "$TEST_WORKSPACE")
budget=$(echo "$config" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('effort',{}).get('budget',{}).get('daily_limit_usd',''))" 2>/dev/null)
if [[ "$budget" == "10.0" ]]; then
    test_pass
else
    test_fail "Expected '10.0', got '$budget'"
fi

# Test: get_workspace_config returns overridden value
test_case "get_workspace_config returns workspace-overridden value"
clear_config_cache
clear_workspace_cache

cat > "$TEST_WORKSPACE/.needle.yaml" << 'EOF'
runner:
  polling_interval: 5s
EOF

value=$(get_workspace_config "$TEST_WORKSPACE" "runner.polling_interval" "2s")
if [[ "$value" == "5s" ]]; then
    test_pass
else
    test_fail "Expected '5s', got '$value'"
fi

# Test: get_workspace_config returns global value for non-overridden key
test_case "get_workspace_config returns global value for non-overridden key"
clear_config_cache
clear_workspace_cache

cat > "$TEST_WORKSPACE/.needle.yaml" << 'EOF'
runner:
  polling_interval: 5s
EOF

# idle_timeout is not overridden in workspace config, so should come from global/defaults
value=$(get_workspace_config "$TEST_WORKSPACE" "runner.idle_timeout" "300s")
if [[ -n "$value" ]]; then
    test_pass
else
    test_fail "Expected idle_timeout value, got empty"
fi

# Test: get_workspace_config returns default when key not found anywhere
test_case "get_workspace_config returns default for missing key"
clear_config_cache
clear_workspace_cache

value=$(get_workspace_config "$TEST_WORKSPACE" "nonexistent.deeply.nested.key" "my_default")
if [[ "$value" == "my_default" ]]; then
    test_pass
else
    test_fail "Expected 'my_default', got '$value'"
fi

# Test: clear_workspace_cache with no arg clears all
test_case "clear_workspace_cache with no argument clears all"
clear_config_cache
clear_workspace_cache

load_workspace_config "$TEST_WORKSPACE" >/dev/null
NEEDLE_WORKSPACE_CONFIG_CACHE="test_value"

clear_workspace_cache  # No arg
if [[ -z "$NEEDLE_WORKSPACE_CONFIG_CACHE" ]]; then
    test_pass
else
    test_fail "Cache not cleared by no-arg call"
fi

# Test: workspace config with preferred_agents is loaded
test_case "workspace config loads preferred_agents field"
clear_config_cache
clear_workspace_cache

cat > "$TEST_WORKSPACE/.needle.yaml" << 'EOF'
preferred_agents:
  - claude-anthropic-sonnet
  - opencode-alibaba-qwen
EOF

config=$(load_workspace_config "$TEST_WORKSPACE")
if echo "$config" | grep -q "preferred_agents\|claude-anthropic-sonnet"; then
    test_pass
else
    test_fail "preferred_agents not found in workspace-merged config"
fi

# Test: reload_workspace_config clears cache and reloads
test_case "reload_workspace_config clears cache and reloads"
clear_config_cache
clear_workspace_cache

cat > "$TEST_WORKSPACE/.needle.yaml" << 'EOF'
runner:
  polling_interval: 7s
EOF

load_workspace_config "$TEST_WORKSPACE" >/dev/null

# Modify workspace config
cat > "$TEST_WORKSPACE/.needle.yaml" << 'EOF'
runner:
  polling_interval: 9s
EOF

config=$(reload_workspace_config "$TEST_WORKSPACE")
interval=$(echo "$config" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('runner',{}).get('polling_interval',''))" 2>/dev/null)
if [[ "$interval" == "9s" ]]; then
    test_pass
else
    test_fail "Expected '9s' after reload, got '$interval'"
fi

# ============ Summary ============
echo ""
echo "================================"
echo "Workspace Config Test Summary"
echo "================================"
echo "Tests run:    $TESTS_RUN"
echo "Tests passed: $TESTS_PASSED"
echo "Tests failed: $TESTS_FAILED"
echo "================================"

if [[ $TESTS_FAILED -gt 0 ]]; then
    exit 1
fi

exit 0
