#!/usr/bin/env bash
# Tests for NEEDLE workspace config loader (src/lib/workspace.sh)

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
source "$PROJECT_DIR/src/lib/config.sh"
source "$PROJECT_DIR/src/lib/workspace.sh"

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
    # Clear workspace cache before each test for isolation
    _NEEDLE_WORKSPACE_CACHE=()
    # Clear global config cache too
    NEEDLE_CONFIG_CACHE=""
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

# ============ Workspace Root Discovery Tests ============

test_case "_needle_find_workspace_root finds .needle.yaml in current dir"
mkdir -p "$TEST_DIR/workspace1"
cat > "$TEST_DIR/workspace1/.needle.yaml" << 'EOF'
name: test-workspace
EOF
result=$(_needle_find_workspace_root "$TEST_DIR/workspace1")
if [[ "$result" == "$TEST_DIR/workspace1" ]]; then
    test_pass
else
    test_fail "Expected '$TEST_DIR/workspace1', got '$result'"
fi

test_case "_needle_find_workspace_root finds .needle.yaml in parent dir"
mkdir -p "$TEST_DIR/workspace2/subdir/deep"
cat > "$TEST_DIR/workspace2/.needle.yaml" << 'EOF'
name: parent-workspace
EOF
result=$(_needle_find_workspace_root "$TEST_DIR/workspace2/subdir/deep")
if [[ "$result" == "$TEST_DIR/workspace2" ]]; then
    test_pass
else
    test_fail "Expected '$TEST_DIR/workspace2', got '$result'"
fi

test_case "_needle_find_workspace_root returns error when no .needle.yaml found"
mkdir -p "$TEST_DIR/noconfig/subdir"
if ! _needle_find_workspace_root "$TEST_DIR/noconfig/subdir" 2>/dev/null; then
    test_pass
else
    test_fail "Expected failure for directory without .needle.yaml"
fi

# ============ Load Workspace Config Tests ============

test_case "load_workspace_config returns global config when no workspace config"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
limits:
  global_max_concurrent: 20
runner:
  polling_interval: 2s
EOF
mkdir -p "$TEST_DIR/no-ws-config"
result=$(load_workspace_config "$TEST_DIR/no-ws-config")
if [[ "$result" == *"global_max_concurrent"* ]]; then
    test_pass
else
    test_fail "Expected global config content, got '$result'"
fi

test_case "load_workspace_config uses workspace config over global"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
limits:
  global_max_concurrent: 20
  default_timeout: 300
EOF
mkdir -p "$TEST_DIR/ws-override"
cat > "$TEST_DIR/ws-override/.needle.yaml" << 'EOF'
limits:
  global_max_concurrent: 5
EOF
result=$(load_workspace_config "$TEST_DIR/ws-override")
if echo "$result" | grep -q "global_max_concurrent.*5"; then
    test_pass
else
    test_fail "Expected workspace override (max_concurrent=5), got '$result'"
fi

test_case "load_workspace_config merges nested structures"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
limits:
  global_max_concurrent: 20
  default_timeout: 300
runner:
  polling_interval: 2s
  idle_timeout: 60s
EOF
mkdir -p "$TEST_DIR/ws-merge"
cat > "$TEST_DIR/ws-merge/.needle.yaml" << 'EOF'
runner:
  idle_timeout: 120s
EOF
result=$(load_workspace_config "$TEST_DIR/ws-merge")
# Should have global_max_concurrent from global AND idle_timeout from workspace
if echo "$result" | grep -q "global_max_concurrent" && echo "$result" | grep -q "idle_timeout"; then
    test_pass
else
    test_fail "Expected merged config with both global and workspace values, got '$result'"
fi

test_case "load_workspace_config caches result"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
name: cached-test
EOF
mkdir -p "$TEST_DIR/ws-cache"
cat > "$TEST_DIR/ws-cache/.needle.yaml" << 'EOF'
name: workspace-cache
EOF
# First call
result1=$(load_workspace_config "$TEST_DIR/ws-cache")
# Second call should return cached result
result2=$(load_workspace_config "$TEST_DIR/ws-cache")
if [[ "$result1" == "$result2" ]]; then
    test_pass
else
    test_fail "Expected cached result to match first call"
fi

test_case "load_workspace_config handles workspace-only config"
# Remove global config
rm -rf "$NEEDLE_HOME"
mkdir -p "$TEST_DIR/ws-only"
cat > "$TEST_DIR/ws-only/.needle.yaml" << 'EOF'
limits:
  max_concurrent: 10
EOF
result=$(load_workspace_config "$TEST_DIR/ws-only")
if [[ "$result" == *"max_concurrent"* ]]; then
    test_pass
else
    test_fail "Expected workspace config content, got '$result'"
fi
# Recreate NEEDLE_HOME for subsequent tests
mkdir -p "$NEEDLE_HOME"

# ============ Get Workspace Setting Tests ============

test_case "get_workspace_setting returns correct value"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
limits:
  max_concurrent: 20
EOF
mkdir -p "$TEST_DIR/ws-setting"
result=$(get_workspace_setting "$TEST_DIR/ws-setting" "limits.max_concurrent" "5")
if [[ "$result" == "20" ]]; then
    test_pass
else
    test_fail "Expected '20', got '$result'"
fi

test_case "get_workspace_setting returns default for missing key"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
name: test
EOF
mkdir -p "$TEST_DIR/ws-default"
result=$(get_workspace_setting "$TEST_DIR/ws-default" "nonexistent.key" "default_value")
if [[ "$result" == "default_value" ]]; then
    test_pass
else
    test_fail "Expected 'default_value', got '$result'"
fi

test_case "get_workspace_setting respects workspace override"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
limits:
  max_concurrent: 20
EOF
mkdir -p "$TEST_DIR/ws-override-setting"
cat > "$TEST_DIR/ws-override-setting/.needle.yaml" << 'EOF'
limits:
  max_concurrent: 5
EOF
result=$(get_workspace_setting "$TEST_DIR/ws-override-setting" "limits.max_concurrent" "10")
if [[ "$result" == "5" ]]; then
    test_pass
else
    test_fail "Expected '5' (workspace override), got '$result'"
fi

test_case "get_workspace_setting_int extracts integer"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
limits:
  max_concurrent: 42
EOF
mkdir -p "$TEST_DIR/ws-int"
result=$(get_workspace_setting_int "$TEST_DIR/ws-int" "limits.max_concurrent" "0")
if [[ "$result" == "42" ]]; then
    test_pass
else
    test_fail "Expected '42', got '$result'"
fi

test_case "get_workspace_setting_bool handles true"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
features:
  enabled: true
EOF
mkdir -p "$TEST_DIR/ws-bool-true"
result=$(get_workspace_setting_bool "$TEST_DIR/ws-bool-true" "features.enabled" "false")
if [[ "$result" == "true" ]]; then
    test_pass
else
    test_fail "Expected 'true', got '$result'"
fi

test_case "get_workspace_setting_bool handles false"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
features:
  enabled: false
EOF
mkdir -p "$TEST_DIR/ws-bool-false"
result=$(get_workspace_setting_bool "$TEST_DIR/ws-bool-false" "features.enabled" "true")
if [[ "$result" == "false" ]]; then
    test_pass
else
    test_fail "Expected 'false', got '$result'"
fi

# ============ Has Workspace Config Tests ============

test_case "has_workspace_config returns true when config exists"
mkdir -p "$TEST_DIR/has-config"
cat > "$TEST_DIR/has-config/.needle.yaml" << 'EOF'
name: has-config
EOF
if has_workspace_config "$TEST_DIR/has-config"; then
    test_pass
else
    test_fail "Expected true for workspace with .needle.yaml"
fi

test_case "has_workspace_config returns false when no config"
mkdir -p "$TEST_DIR/no-config"
if ! has_workspace_config "$TEST_DIR/no-config" 2>/dev/null; then
    test_pass
else
    test_fail "Expected false for workspace without .needle.yaml"
fi

# ============ Get Workspace Config Path Tests ============

test_case "get_workspace_config_path returns correct path"
mkdir -p "$TEST_DIR/ws-path"
cat > "$TEST_DIR/ws-path/.needle.yaml" << 'EOF'
name: path-test
EOF
result=$(get_workspace_config_path "$TEST_DIR/ws-path")
if [[ "$result" == "$TEST_DIR/ws-path/.needle.yaml" ]]; then
    test_pass
else
    test_fail "Expected '$TEST_DIR/ws-path/.needle.yaml', got '$result'"
fi

test_case "get_workspace_config_path finds parent config"
mkdir -p "$TEST_DIR/ws-parent/subdir/deep"
cat > "$TEST_DIR/ws-parent/.needle.yaml" << 'EOF'
name: parent-test
EOF
result=$(get_workspace_config_path "$TEST_DIR/ws-parent/subdir/deep")
if [[ "$result" == "$TEST_DIR/ws-parent/.needle.yaml" ]]; then
    test_pass
else
    test_fail "Expected parent config path, got '$result'"
fi

# ============ Cache Management Tests ============

test_case "clear_workspace_cache clears specific workspace"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
name: cache-clear
EOF
mkdir -p "$TEST_DIR/ws-cache-clear"
# Load to cache
load_workspace_config "$TEST_DIR/ws-cache-clear" >/dev/null
# Clear cache
clear_workspace_cache "$TEST_DIR/ws-cache-clear"
# Verify cache was cleared (check internal array)
if [[ -z "${_NEEDLE_WORKSPACE_CACHE[$TEST_DIR/ws-cache-clear]:-}" ]]; then
    test_pass
else
    test_fail "Expected cache to be cleared"
fi

test_case "clear_workspace_cache with no args clears all"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
name: cache-all
EOF
mkdir -p "$TEST_DIR/ws-cache-1" "$TEST_DIR/ws-cache-2"
# Load both to cache
load_workspace_config "$TEST_DIR/ws-cache-1" >/dev/null
load_workspace_config "$TEST_DIR/ws-cache-2" >/dev/null
# Clear all
clear_workspace_cache
# Verify all cache cleared
if [[ ${#_NEEDLE_WORKSPACE_CACHE[@]} -eq 0 ]]; then
    test_pass
else
    test_fail "Expected all cache to be cleared"
fi

test_case "reload_workspace_config clears cache and reloads"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
version: 1
EOF
mkdir -p "$TEST_DIR/ws-reload"
cat > "$TEST_DIR/ws-reload/.needle.yaml" << 'EOF'
version: 1
EOF
# Load to cache
load_workspace_config "$TEST_DIR/ws-reload" >/dev/null
# Modify config
cat > "$TEST_DIR/ws-reload/.needle.yaml" << 'EOF'
version: 2
EOF
# Reload
result=$(reload_workspace_config "$TEST_DIR/ws-reload")
if [[ "$result" == *"version"* ]]; then
    test_pass
else
    test_fail "Expected reloaded config with version"
fi

test_case "list_cached_workspaces shows cached entries"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
name: list-test
EOF
mkdir -p "$TEST_DIR/ws-list-1" "$TEST_DIR/ws-list-2"
# Load both to cache
load_workspace_config "$TEST_DIR/ws-list-1" >/dev/null
load_workspace_config "$TEST_DIR/ws-list-2" >/dev/null
result=$(list_cached_workspaces)
if [[ "$result" == *"$TEST_DIR/ws-list-1"* ]] && [[ "$result" == *"$TEST_DIR/ws-list-2"* ]]; then
    test_pass
else
    test_fail "Expected both workspaces in list, got '$result'"
fi

# ============ Edge Cases ============

test_case "load_workspace_config handles invalid workspace path"
if ! load_workspace_config "/nonexistent/path/xyz" 2>/dev/null; then
    test_pass
else
    test_fail "Expected failure for invalid workspace path"
fi

test_case "load_workspace_config handles empty workspace config"
mkdir -p "$NEEDLE_HOME"
cat > "$NEEDLE_HOME/config.yaml" << 'EOF'
name: empty-fallback
EOF
mkdir -p "$TEST_DIR/ws-empty"
touch "$TEST_DIR/ws-empty/.needle.yaml"
result=$(load_workspace_config "$TEST_DIR/ws-empty" 2>/dev/null)
# Should handle gracefully - either return empty or global fallback
# Just check it doesn't crash
if [[ -n "$result" ]] || [[ $? -eq 0 ]]; then
    test_pass
else
    test_fail "Expected graceful handling of empty config"
fi

test_case "_needle_config_extract_value extracts simple value"
config="name: test-value
other: data"
result=$(_needle_config_extract_value "$config" "name")
if [[ "$result" == "test-value" ]]; then
    test_pass
else
    test_fail "Expected 'test-value', got '$result'"
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
