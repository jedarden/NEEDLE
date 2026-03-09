#!/usr/bin/env bash
# Tests for NEEDLE intent declaration module (src/bead/intent.sh)

# Test setup - create temp directory
TEST_DIR=$(mktemp -d)
TEST_WORKSPACE="$TEST_DIR/workspace"
mkdir -p "$TEST_WORKSPACE"

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_HOME="$TEST_DIR/.needle"
export NEEDLE_STATE_DIR="state"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=false
export NEEDLE_LOCK_DIR="$TEST_DIR/locks"
mkdir -p "$NEEDLE_LOCK_DIR"

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/lock/checkout.sh"
source "$PROJECT_DIR/src/bead/intent.sh"

# Initialize beads database for testing
export BR_DB="$TEST_DIR/test.db"
br init --db "$BR_DB" --quiet 2>/dev/null || true

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
    local reason="${1:-unknown}"
    echo "FAIL: $reason"
    ((TESTS_FAILED++))
}

assert_equals() {
    local expected="$1"
    local actual="$2"
    local msg="${3:-}"
    if [[ "$expected" == "$actual" ]]; then
        return 0
    else
        echo "  Expected: '$expected'" >&2
        echo "  Got: '$actual'" >&2
        return 1
    fi
}

assert_contains() {
    local haystack="$1"
    local needle="$2"
    if echo "$haystack" | grep -qF "$needle"; then
        return 0
    else
        echo "  '$haystack' does not contain '$needle'" >&2
        return 1
    fi
}

# ============================================================================
# Test: _needle_extract_files_from_description
# ============================================================================

test_case "extract_files_from_description: single file path"
result=$(_needle_extract_files_from_description "Fix bug in src/cli/run.sh where parse_args fails")
if assert_contains "$result" "src/cli/run.sh"; then
    test_pass
else
    test_fail "expected src/cli/run.sh in result"
fi

test_case "extract_files_from_description: multiple file paths"
desc="Update src/lib/config.sh and src/lib/output.sh for consistency"
result=$(_needle_extract_files_from_description "$desc")
if assert_contains "$result" "src/lib/config.sh" && assert_contains "$result" "src/lib/output.sh"; then
    test_pass
else
    test_fail "expected both files in result"
fi

test_case "extract_files_from_description: absolute path"
result=$(_needle_extract_files_from_description "Fix /home/coder/project/src/test.sh")
# Check that result contains the path (with or without leading slash due to word boundary)
if echo "$result" | grep -q "home/coder/project/src/test.sh"; then
    test_pass
else
    test_fail "expected absolute path, got: '$result'"
fi

test_case "extract_files_from_description: various extensions"
desc="Files: src/main.ts, config.yaml, data.json, README.md"
result=$(_needle_extract_files_from_description "$desc")
if echo "$result" | grep -q "src/main.ts" && \
   echo "$result" | grep -q "config.yaml" && \
   echo "$result" | grep -q "data.json" && \
   echo "$result" | grep -q "README.md"; then
    test_pass
else
    test_fail "expected all extensions"
fi

test_case "extract_files_from_description: no files in text"
result=$(_needle_extract_files_from_description "Just a regular description without file paths")
if [[ -z "$result" ]]; then
    test_pass
else
    test_fail "expected empty result"
fi

test_case "extract_files_from_description: empty input"
result=$(_needle_extract_files_from_description "")
if [[ -z "$result" ]]; then
    test_pass
else
    test_fail "expected empty result"
fi

test_case "extract_files_from_description: Python files"
result=$(_needle_extract_files_from_description "Fix src/lib/client.py and tests/test_client.py")
if assert_contains "$result" "src/lib/client.py" && assert_contains "$result" "tests/test_client.py"; then
    test_pass
else
    test_fail "expected Python files"
fi

test_case "extract_files_from_description: Rust files"
result=$(_needle_extract_files_from_description "Update src/main.rs and src/lib/helper.rs")
if assert_contains "$result" "src/main.rs" && assert_contains "$result" "src/lib/helper.rs"; then
    test_pass
else
    test_fail "expected Rust files"
fi

test_case "extract_files_from_description: duplicate removal"
desc="Fix src/cli/run.sh which mentions src/cli/run.sh again"
result=$(_needle_extract_files_from_description "$desc")
count=$(echo "$result" | grep -c "src/cli/run.sh" || true)
if [[ $count -eq 1 ]]; then
    test_pass
else
    test_fail "expected unique files only, got $count occurrences"
fi

# ============================================================================
# Test: _needle_extract_files_from_label
# ============================================================================

test_case "extract_files_from_label: files label present"
# Mock bead JSON with files label
mock_json='[{"id":"nd-123","labels":["files:src/a.sh,src/b.sh","bug"]}]'
result=$(_needle_extract_files_from_label "$mock_json")
if [[ "$result" == "src/a.sh,src/b.sh" ]]; then
    test_pass
else
    test_fail "expected 'src/a.sh,src/b.sh', got '$result'"
fi

test_case "extract_files_from_label: no files label"
mock_json='[{"id":"nd-123","labels":["bug","urgent"]}]'
result=$(_needle_extract_files_from_label "$mock_json")
if [[ -z "$result" ]]; then
    test_pass
else
    test_fail "expected empty result"
fi

test_case "extract_files_from_label: empty labels"
mock_json='[{"id":"nd-123"}]'
result=$(_needle_extract_files_from_label "$mock_json")
if [[ -z "$result" ]]; then
    test_pass
else
    test_fail "expected empty result"
fi

test_case "extract_files_from_label: single file in label"
mock_json='[{"id":"nd-123","labels":["files:src/main.sh"]}]'
result=$(_needle_extract_files_from_label "$mock_json")
if [[ "$result" == "src/main.sh" ]]; then
    test_pass
else
    test_fail "expected 'src/main.sh', got '$result'"
fi

# ============================================================================
# Test: File Lock Integration
# ============================================================================

test_case "checkout_file: can checkout a file"
# Create a test file
test_file="$TEST_WORKSPACE/test.txt"
touch "$test_file"

# Try to checkout
if checkout_file "$test_file" "test-nd-123" "test-worker"; then
    # Verify lock exists using check_file
    if check_file "$test_file" 2>/dev/null; then
        release_file "$test_file" "test-nd-123"
        test_pass
    else
        release_file "$test_file" "test-nd-123"
        test_fail "lock not detected by check_file"
    fi
else
    test_fail "checkout failed"
fi
rm -f "$test_file"

test_case "checkout_file: conflict detection"
# Create a test file
test_file="$TEST_WORKSPACE/test.txt"
touch "$test_file"

# First checkout should succeed
if checkout_file "$test_file" "test-nd-123" "worker1"; then
    # Second checkout with different bead should fail
    if checkout_file "$test_file" "test-nd-456" "worker2"; then
        release_file "$test_file" "test-nd-123"
        test_fail "second checkout should have failed"
    else
        release_file "$test_file" "test-nd-123"
        test_pass
    fi
else
    test_fail "first checkout failed"
fi
rm -f "$test_file"

test_case "checkout_file: same bead can re-checkout"
test_file="$TEST_WORKSPACE/test.txt"
touch "$test_file"

if checkout_file "$test_file" "test-nd-123" "worker1"; then
    # Re-checkout with same bead should succeed
    if checkout_file "$test_file" "test-nd-123" "worker1"; then
        release_file "$test_file" "test-nd-123"
        test_pass
    else
        release_file "$test_file" "test-nd-123"
        test_fail "re-checkout failed"
    fi
else
    test_fail "first checkout failed"
fi
rm -f "$test_file"

# ============================================================================
# Test: _needle_claim_with_intent (mocked)
# ============================================================================

test_case "claim_with_intent: no files declared returns exit 2"
# When intent is disabled, should return 2
NEEDLE_INTENT_ENABLED=false
_needle_claim_with_intent "test-nd-123" --actor "test-worker"
result=$?
if [[ $result -eq 2 ]]; then
    test_pass
else
    test_fail "expected exit code 2, got $result"
fi
NEEDLE_INTENT_ENABLED=true

# ============================================================================
# Summary
# ============================================================================

echo ""
echo "=========================================="
echo "Test Results:"
echo "  Run:     $TESTS_RUN"
echo "  Passed:  $TESTS_PASSED"
echo "  Failed:  $TESTS_FAILED"
echo "=========================================="

if [[ $TESTS_FAILED -gt 0 ]]; then
    exit 1
fi

exit 0
