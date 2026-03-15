#!/usr/bin/env bash
# Tests for NEEDLE mitosis configuration (src/bead/mitosis.sh config integration)

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
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=false

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/json.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/lib/config.sh"
source "$PROJECT_DIR/src/lib/workspace.sh"
source "$PROJECT_DIR/src/bead/mitosis.sh"

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
    # Clear caches before each test
    NEEDLE_CONFIG_CACHE=""
    _NEEDLE_WORKSPACE_CACHE=()
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

# ============ Global Config Tests ============

test_case "_needle_mitosis_config reads from global config"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
  max_children: 3
  min_complexity: 50
  skip_types: bug,hotfix
  skip_labels: atomic
EOF
value=$(_needle_mitosis_config "max_children" "5")
if [[ "$value" == "3" ]]; then
    test_pass
else
    test_fail "Expected '3', got '$value'"
fi

test_case "_needle_mitosis_config returns default for missing key"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
EOF
value=$(_needle_mitosis_config "nonexistent" "default_value")
if [[ "$value" == "default_value" ]]; then
    test_pass
else
    test_fail "Expected 'default_value', got '$value'"
fi

test_case "_needle_mitosis_is_enabled returns true when enabled"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
EOF
if _needle_mitosis_is_enabled; then
    test_pass
else
    test_fail "Expected mitosis to be enabled"
fi

test_case "_needle_mitosis_is_enabled returns false when disabled"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: false
EOF
if ! _needle_mitosis_is_enabled; then
    test_pass
else
    test_fail "Expected mitosis to be disabled"
fi

# ============ Workspace Config Override Tests ============

test_case "_needle_mitosis_config uses workspace override"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
  max_children: 5
EOF
mkdir -p "$TEST_DIR/workspace1"
cat > "$TEST_DIR/workspace1/.needle.yaml" << 'EOF'
mitosis:
  max_children: 2
EOF
value=$(_needle_mitosis_config "max_children" "10" "$TEST_DIR/workspace1")
if [[ "$value" == "2" ]]; then
    test_pass
else
    test_fail "Expected workspace override '2', got '$value'"
fi

test_case "_needle_mitosis_config falls back to global when workspace has no override"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
  max_children: 5
EOF
mkdir -p "$TEST_DIR/workspace2"
cat > "$TEST_DIR/workspace2/.needle.yaml" << 'EOF'
name: test-workspace
EOF
value=$(_needle_mitosis_config "max_children" "10" "$TEST_DIR/workspace2")
if [[ "$value" == "5" ]]; then
    test_pass
else
    test_fail "Expected global value '5', got '$value'"
fi

test_case "_needle_mitosis_is_enabled respects workspace override"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
EOF
mkdir -p "$TEST_DIR/workspace3"
cat > "$TEST_DIR/workspace3/.needle.yaml" << 'EOF'
mitosis:
  enabled: false
EOF
if ! _needle_mitosis_is_enabled "$TEST_DIR/workspace3"; then
    test_pass
else
    test_fail "Expected workspace override to disable mitosis"
fi

# ============ Skip Types Tests ============

test_case "_needle_mitosis_get_skip_types returns configured types"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  skip_types: bug,hotfix,incident
EOF
value=$(_needle_mitosis_get_skip_types)
if [[ "$value" == "bug,hotfix,incident" ]]; then
    test_pass
else
    test_fail "Expected 'bug,hotfix,incident', got '$value'"
fi

test_case "_needle_mitosis_get_skip_types respects workspace override"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  skip_types: bug,hotfix
EOF
mkdir -p "$TEST_DIR/workspace4"
cat > "$TEST_DIR/workspace4/.needle.yaml" << 'EOF'
mitosis:
  skip_types: bug,hotfix,incident,feature
EOF
value=$(_needle_mitosis_get_skip_types "$TEST_DIR/workspace4")
if [[ "$value" == "bug,hotfix,incident,feature" ]]; then
    test_pass
else
    test_fail "Expected workspace override, got '$value'"
fi

# ============ Skip Labels Tests ============

test_case "_needle_mitosis_get_skip_labels returns configured labels"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  skip_labels: no-mitosis,atomic
EOF
value=$(_needle_mitosis_get_skip_labels)
if [[ "$value" == "no-mitosis,atomic" ]]; then
    test_pass
else
    test_fail "Expected 'no-mitosis,atomic', got '$value'"
fi

# ============ Min Complexity Tests ============

test_case "_needle_mitosis_get_min_complexity returns configured value"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  min_complexity: 75
EOF
value=$(_needle_mitosis_get_min_complexity)
if [[ "$value" == "75" ]]; then
    test_pass
else
    test_fail "Expected '75', got '$value'"
fi

test_case "_needle_mitosis_get_min_complexity respects workspace override"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  min_complexity: 100
EOF
mkdir -p "$TEST_DIR/workspace5"
cat > "$TEST_DIR/workspace5/.needle.yaml" << 'EOF'
mitosis:
  min_complexity: 25
EOF
value=$(_needle_mitosis_get_min_complexity "$TEST_DIR/workspace5")
if [[ "$value" == "25" ]]; then
    test_pass
else
    test_fail "Expected workspace override '25', got '$value'"
fi

# ============ Max Children Validation Tests ============

test_case "validate_config passes valid max_children"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  max_children: 10
EOF
if validate_config "$TEST_CONFIG_FILE" 2>/dev/null; then
    test_pass
else
    test_fail "Valid max_children should pass validation"
fi

test_case "validate_config fails for max_children = 0"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  max_children: 0
EOF
if ! validate_config "$TEST_CONFIG_FILE" 2>/dev/null; then
    test_pass
else
    test_fail "max_children=0 should fail validation"
fi

test_case "validate_config fails for negative max_children"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  max_children: -5
EOF
if ! validate_config "$TEST_CONFIG_FILE" 2>/dev/null; then
    test_pass
else
    test_fail "Negative max_children should fail validation"
fi

# ============ Min Complexity Validation Tests ============

test_case "validate_config passes valid min_complexity"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  min_complexity: 50
EOF
if validate_config "$TEST_CONFIG_FILE" 2>/dev/null; then
    test_pass
else
    test_fail "Valid min_complexity should pass validation"
fi

test_case "validate_config passes min_complexity = 0"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  min_complexity: 0
EOF
if validate_config "$TEST_CONFIG_FILE" 2>/dev/null; then
    test_pass
else
    test_fail "min_complexity=0 should pass validation (non-negative)"
fi

test_case "validate_config fails for negative min_complexity"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  min_complexity: -10
EOF
if ! validate_config "$TEST_CONFIG_FILE" 2>/dev/null; then
    test_pass
else
    test_fail "Negative min_complexity should fail validation"
fi

# ============ Environment Variable Fallback Tests ============
# Note: Environment variables are used as the default parameter when calling
# _needle_mitosis_config, but config defaults always have values, so env vars
# only apply when the config explicitly doesn't have a value (not the default case).
# The defaults in config.sh always include mitosis settings.

test_case "_needle_mitosis_config uses provided default when config missing key"
# Create config without mitosis section at all
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
name: no-mitosis-section
EOF
# Clear cache to reload
NEEDLE_CONFIG_CACHE=""
# The default config will still have mitosis defaults, so we test with a non-existent key
value=$(_needle_mitosis_config "nonexistent_setting" "custom_default")
if [[ "$value" == "custom_default" ]]; then
    test_pass
else
    test_fail "Expected custom_default for nonexistent key, got '$value'"
fi

# ============ Force On Failure Config Tests ============

test_case "_needle_mitosis_force_enabled returns true when force_on_failure=true"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
  force_on_failure: true
  force_failure_threshold: 3
EOF
if _needle_mitosis_force_enabled; then
    test_pass
else
    test_fail "Expected force_on_failure to be enabled"
fi

test_case "_needle_mitosis_force_enabled returns false when force_on_failure=false"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
  force_on_failure: false
EOF
if ! _needle_mitosis_force_enabled; then
    test_pass
else
    test_fail "Expected force_on_failure to be disabled"
fi

test_case "_needle_mitosis_force_threshold returns configured value"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
  force_failure_threshold: 5
EOF
value=$(_needle_mitosis_force_threshold)
if [[ "$value" == "5" ]]; then
    test_pass
else
    test_fail "Expected '5', got '$value'"
fi

test_case "_needle_mitosis_force_threshold returns default when not configured"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
EOF
value=$(_needle_mitosis_force_threshold)
if [[ "$value" == "3" ]]; then
    test_pass
else
    test_fail "Expected default '3', got '$value'"
fi

test_case "_needle_mitosis_force_enabled respects workspace override"
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
  force_on_failure: true
EOF
mkdir -p "$TEST_DIR/workspace_force"
cat > "$TEST_DIR/workspace_force/.needle.yaml" << 'EOF'
mitosis:
  force_on_failure: false
EOF
if ! _needle_mitosis_force_enabled "$TEST_DIR/workspace_force"; then
    test_pass
else
    test_fail "Expected workspace override to disable force_on_failure"
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
