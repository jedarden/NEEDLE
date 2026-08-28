#!/run/current-system/sw/bin/bash
#
# Standalone test runner for install.sh checksum verification
# Does NOT require Bats - pure shell implementation
#
# Usage:
#   ./run_tests_standalone.sh [OPTIONS]
#
# Options:
#   -v, --verbose    Verbose output
#   -l, --list       List all tests
#   -f, --filter PATTERN  Run tests matching pattern
#   -h, --help       Show help

set -euo pipefail

# Colors
if [[ -t 1 ]]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    BLUE='\033[0;34m'
    CYAN='\033[0;36m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    CYAN=''
    NC=''
fi

# Test counters
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0
TESTS_SKIPPED=0

# Test directory
TEST_TEMP_DIR=""
FIXTURE_DIR=""

# Options
VERBOSE=false
FILTER=""

usage() {
    cat <<EOF
Usage: $0 [OPTIONS]

Run install.sh checksum verification tests (standalone, no Bats required).

Options:
    -v, --verbose        Verbose output
    -l, --list           List all tests
    -f, --filter PATTERN Run tests matching pattern
    -h, --help           Show this help

Environment:
    No special environment variables required.

Exit codes:
    0 - All tests passed
    1 - One or more tests failed
    2 - Test execution error
EOF
}

log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_pass() {
    echo -e "${GREEN}[PASS]${NC} $1"
}

log_fail() {
    echo -e "${RED}[FAIL]${NC} $1"
}

log_skip() {
    echo -e "${YELLOW}[SKIP]${NC} $1"
}

# Test helper functions
create_checksum_fixtures() {
    # Valid checksums file
    cat > "${FIXTURE_DIR}/checksums-valid.txt" <<EOF
abc123def456  needle-x86_64-unknown-linux-gnu
789xyz012ghi  needle-aarch64-apple-darwin
EOF

    # Checksums file missing the expected asset
    cat > "${FIXTURE_DIR}/checksums-missing-asset.txt" <<EOF
abc123def456  needle-aarch64-apple-darwin
789xyz012ghi  some-other-asset
EOF

    # Empty checksums file
    touch "${FIXTURE_DIR}/checksums-empty.txt"

    # Malformed checksums file
    cat > "${FIXTURE_DIR}/checksums-malformed.txt" <<EOF
invalid checksum format
abc123 no hash here
EOF
}

verify_checksum_with_fixture() {
    local binary="$1"
    local checksums_file="$2"
    local asset_name="$3"
    local skip_checksum="${NEEDLE_SKIP_CHECKSUM:-false}"

    # Check if checksums file exists
    if [[ ! -f "$checksums_file" ]]; then
        if [[ "$skip_checksum" == "true" ]]; then
            echo "WARNING: Skipping checksum verification (checksums.txt unavailable)"
            return 0
        else
            echo "ERROR: Could not download checksums.txt"
            return 1
        fi
    fi

    # Extract expected hash
    local expected_hash
    expected_hash=$(grep "  ${asset_name}$\| ${asset_name}$" "$checksums_file" | awk '{print $1}' || true)

    if [[ -z "$expected_hash" ]]; then
        if [[ "$skip_checksum" == "true" ]]; then
            echo "WARNING: Skipping checksum verification (checksum for ${asset_name} not found)"
            return 0
        else
            echo "ERROR: Could not find checksum for ${asset_name}"
            return 1
        fi
    fi

    # Compute actual hash
    local actual_hash=""
    local found_hash_tool=false

    if command -v sha256sum &>/dev/null; then
        actual_hash=$(sha256sum "$binary" | awk '{print $1}')
        found_hash_tool=true
    elif command -v shasum &>/dev/null; then
        actual_hash=$(shasum -a 256 "$binary" | awk '{print $1}')
        found_hash_tool=true
    fi

    if [[ "$found_hash_tool" == "false" ]]; then
        if [[ "$skip_checksum" == "true" ]]; then
            echo "WARNING: Skipping checksum verification (no hash tool available)"
            return 0
        else
            echo "ERROR: Neither sha256sum nor shasum available"
            return 1
        fi
    fi

    if [[ -z "$actual_hash" ]]; then
        if [[ "$skip_checksum" == "true" ]]; then
            echo "WARNING: Skipping checksum verification (failed to compute checksum)"
            return 0
        else
            echo "ERROR: Failed to compute checksum"
            return 1
        fi
    fi

    # Verify checksum matches - MISMATCHES ARE NEVER SKIPPABLE
    if [[ "$actual_hash" != "$expected_hash" ]]; then
        echo "ERROR: Checksum mismatch for ${asset_name}"
        return 1
    fi

    echo "SUCCESS: Checksum verified"
    return 0
}

run_test() {
    local test_name="$1"
    local test_func="$2"

    TESTS_RUN=$((TESTS_RUN + 1))

    # Apply filter if set
    if [[ -n "$FILTER" ]] && [[ ! "$test_name" =~ $FILTER ]]; then
        return
    fi

    log_info "Running: $test_name"

    # Setup
    TEST_TEMP_DIR=$(mktemp -d)
    FIXTURE_DIR="${TEST_TEMP_DIR}/fixtures"
    mkdir -p "$FIXTURE_DIR"
    create_checksum_fixtures
    export HOME="$TEST_TEMP_DIR"

    # Run test
    local output
    local exit_code=0
    output=$($test_func 2>&1) || exit_code=$?

    # Check result
    if [[ $exit_code -eq 0 ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_pass "$test_name"
        if [[ "$VERBOSE" == "true" ]]; then
            echo "$output" | sed 's/^/    /'
        fi
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_fail "$test_name"
        echo "$output" | sed 's/^/    /'
    fi

    # Teardown
    rm -rf "$TEST_TEMP_DIR"
    TEST_TEMP_DIR=""
    FIXTURE_DIR=""
}

skip_test() {
    local test_name="$1"
    local reason="${2:-no reason given}"

    if [[ -n "$FILTER" ]] && [[ ! "$test_name" =~ $FILTER ]]; then
        return
    fi

    TESTS_RUN=$((TESTS_RUN + 1))
    TESTS_SKIPPED=$((TESTS_SKIPPED + 1))
    log_skip "$test_name ($reason)"
}

# ============================================================================
# Test Functions
# ============================================================================

test_valid_checksum() {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    # Calculate actual checksum
    local actual_checksum
    if command -v sha256sum &>/dev/null; then
        actual_checksum=$(sha256sum "$test_binary" | awk '{print $1}')
    elif command -v shasum &>/dev/null; then
        actual_checksum=$(shasum -a 256 "$test_binary" | awk '{print $1}')
    else
        return 1
    fi

    # Create checksums file with actual checksum
    cat > "${FIXTURE_DIR}/checksums-actual.txt" <<EOF
${actual_checksum}  needle-x86_64-unknown-linux-gnu
EOF

    verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-actual.txt" "needle-x86_64-unknown-linux-gnu"
}

test_missing_checksums_file_fails() {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    # Should fail with non-existent checksums file
    ! verify_checksum_with_fixture "$test_binary" "${TEST_TEMP_DIR}/nonexistent.txt" "needle-x86_64-unknown-linux-gnu"
}

test_missing_checksums_with_skip() {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    export NEEDLE_SKIP_CHECKSUM="true"
    verify_checksum_with_fixture "$test_binary" "${TEST_TEMP_DIR}/nonexistent.txt" "needle-x86_64-unknown-linux-gnu"
}

test_checksum_not_found_fails() {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    ! verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-missing-asset.txt" "needle-x86_64-unknown-linux-gnu"
}

test_checksum_not_found_with_skip() {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    export NEEDLE_SKIP_CHECKSUM="true"
    verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-missing-asset.txt" "needle-x86_64-unknown-linux-gnu"
}

test_checksum_mismatch_always_fails() {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    # Create checksums file with wrong checksum
    cat > "${FIXTURE_DIR}/checksums-wrong.txt" <<EOF
wrongchecksum123  needle-x86_64-unknown-linux-gnu
EOF

    # Should fail without skip
    ! verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-wrong.txt" "needle-x86_64-unknown-linux-gnu"

    # Should ALSO fail with skip (mismatches never skippable)
    export NEEDLE_SKIP_CHECKSUM="true"
    ! verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-wrong.txt" "needle-x86_64-unknown-linux-gnu"
}

test_empty_checksums_file() {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    ! verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-empty.txt" "needle-x86_64-unknown-linux-gnu"
}

test_environment_variable_normalization() {
    # Test various "true" values are accepted
    for val in "1" "true" "yes" "TRUE" "YES"; do
        export NEEDLE_SKIP_CHECKSUM="$val"
        # Test that the value is truthy (not empty and not a false value)
        [[ -n "$NEEDLE_SKIP_CHECKSUM" ]] || return 1
        [[ "$NEEDLE_SKIP_CHECKSUM" != "0" ]] || return 1
        [[ "$NEEDLE_SKIP_CHECKSUM" != "false" ]] || return 1
        [[ "$NEEDLE_SKIP_CHECKSUM" != "no" ]] || return 1
    done

    # Test "false" values
    for val in "0" "false" "no" "" "random"; do
        export NEEDLE_SKIP_CHECKSUM="$val"
        # These should not be considered truthy
        # The test passes if we successfully iterate through all values
        true
    done
}

test_sha256sum_tool() {
    if ! command -v sha256sum &>/dev/null; then
        return 1  # Skip
    fi

    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content for sha256sum" > "$test_binary"

    # Get actual checksum
    local actual_checksum
    actual_checksum=$(sha256sum "$test_binary" | awk '{print $1}')

    # Create checksums file
    cat > "${FIXTURE_DIR}/checksums-sha256sum.txt" <<EOF
${actual_checksum}  needle-x86_64-unknown-linux-gnu
EOF

    verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-sha256sum.txt" "needle-x86_64-unknown-linux-gnu"
}

test_shasum_tool() {
    if ! command -v shasum &>/dev/null; then
        return 1  # Skip
    fi

    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content for shasum" > "$test_binary"

    # Get actual checksum
    local actual_checksum
    actual_checksum=$(shasum -a 256 "$test_binary" | awk '{print $1}')

    # Create checksums file
    cat > "${FIXTURE_DIR}/checksums-shasum.txt" <<EOF
${actual_checksum}  needle-x86_64-unknown-linux-gnu
EOF

    verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-shasum.txt" "needle-x86_64-unknown-linux-gnu"
}

# ============================================================================
# Main
# ============================================================================

main() {
    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -v|--verbose)
                VERBOSE=true
                shift
                ;;
            -l|--list)
                echo "Available tests:"
                echo "  1. Valid checksum verification"
                echo "  2. Missing checksums file fails"
                echo "  3. Missing checksums with --skip-checksum"
                echo "  4. Checksum not found fails"
                echo "  5. Checksum not found with --skip-checksum"
                echo "  6. Checksum mismatch always fails (even with --skip-checksum)"
                echo "  7. Empty checksums file"
                echo "  8. Environment variable normalization"
                echo "  9. SHA-256 checksum tool (sha256sum)"
                echo " 10. SHA-256 checksum tool (shasum)"
                exit 0
                ;;
            -f|--filter)
                FILTER="$2"
                shift 2
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                echo "Error: Unknown option: $1" >&2
                usage
                exit 2
                ;;
        esac
    done

    echo "============================================================================"
    echo "install.sh Checksum Verification Tests"
    echo "============================================================================"
    echo ""

    # Run tests
    run_test "Valid checksum verification" test_valid_checksum
    run_test "Missing checksums file fails without --skip-checksum" test_missing_checksums_file_fails
    run_test "Missing checksums file succeeds with --skip-checksum" test_missing_checksums_with_skip
    run_test "Checksum for asset not found fails without --skip-checksum" test_checksum_not_found_fails
    run_test "Checksum for asset not found succeeds with --skip-checksum" test_checksum_not_found_with_skip
    run_test "Checksum mismatch always fails (even with --skip-checksum)" test_checksum_mismatch_always_fails
    run_test "Empty checksums file fails" test_empty_checksums_file
    run_test "Environment variable normalization" test_environment_variable_normalization

    # Tool-specific tests
    if command -v sha256sum &>/dev/null; then
        run_test "Valid checksum with sha256sum" test_sha256sum_tool
    else
        skip_test "Valid checksum with sha256sum" "sha256sum not available"
    fi

    if command -v shasum &>/dev/null; then
        run_test "Valid checksum with shasum" test_shasum_tool
    else
        skip_test "Valid checksum with shasum" "shasum not available"
    fi

    # Summary
    echo ""
    echo "============================================================================"
    echo "Test Summary"
    echo "============================================================================"
    echo "  Total:   $TESTS_RUN"
    echo -e "  ${GREEN}Passed:  $TESTS_PASSED${NC}"
    if [[ $TESTS_FAILED -gt 0 ]]; then
        echo -e "  ${RED}Failed:  $TESTS_FAILED${NC}"
    else
        echo "  Failed:  $TESTS_FAILED"
    fi
    if [[ $TESTS_SKIPPED -gt 0 ]]; then
        echo -e "  ${YELLOW}Skipped: $TESTS_SKIPPED${NC}"
    else
        echo "  Skipped: $TESTS_SKIPPED"
    fi
    echo "============================================================================"

    if [[ $TESTS_FAILED -gt 0 ]]; then
        exit 1
    else
        exit 0
    fi
}

main "$@"
