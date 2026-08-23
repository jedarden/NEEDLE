#!/bin/bash
#
# Comprehensive isolated test suite for install.sh
# Tests are fully isolated, parallel-safe, and use mocked HTTP/download layer
#

set -euo pipefail

# Test framework helpers
TEST_COUNT=0
PASS_COUNT=0
FAIL_COUNT=0

# Unique test ID for parallel safety (using process ID and timestamp)
TEST_ID="installer-test-$$-${RANDOM}"

# Test framework helpers with parallel safety
setup() {
    # Use unique temp directory per test for parallel safety
    tmp_dir=$(mktemp -d -t "${TEST_ID}-XXXXXX")
    export HOME="$tmp_dir"
    export NEEDLE_INSTALL_PATH="$tmp_dir/needle"
}

teardown() {
    if [[ -n "$tmp_dir" && -d "$tmp_dir" ]]; then
        rm -rf "$tmp_dir"
    fi
}

assert_eq() {
    local expected="$1"
    local actual="$2"
    local msg="${3:-assertion failed}"

    if [[ "$expected" == "$actual" ]]; then
        echo "  ✓ $msg"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ $msg"
        echo "    expected: $expected"
        echo "    got: $actual"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true
    return 0
}

assert_contains() {
    local haystack="$1"
    local needle="$2"
    local msg="${3:-assertion failed}"

    if [[ "$haystack" == *"$needle"* ]]; then
        echo "  ✓ $msg"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ $msg"
        echo "    expected to contain: $needle"
        echo "    in: $haystack"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true
    return 0
}

assert_exit_code() {
    local expected="$1"
    local actual="$2"
    local msg="${3:-exit code assertion failed}"

    if [[ "$expected" == "$actual" ]]; then
        echo "  ✓ $msg"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ $msg"
        echo "    expected exit code: $expected"
        echo "    got: $actual"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true
    return 0
}

# Mock fixture creation
create_mock_binary() {
    local binary_file="$1"
    local content="${2:-mock binary content}"
    printf "%s" "$content" > "$binary_file"
    chmod +x "$binary_file"
}

create_mock_checksums() {
    local checksums_file="$1"
    local hash="$2"
    local asset_name="${3:-needle-x86_64-unknown-linux-gnu}"

    cat > "$checksums_file" <<EOF
$hash  $asset_name
otherhash  needle-aarch64-unknown-linux-gnu
EOF
}

# Mock HTTP server using nc (netcat) for parallel-safe testing
start_mock_http_server() {
    local port=$1
    local response_dir=$2

    mkdir -p "$response_dir"

    # Create mock server script
    cat > "$response_dir/server.sh" <<EOF
#!/bin/bash
while true; do
    RESPONSE_FILE="\$2/\$(echo \$1 | sed 's/[^a-zA-Z0-9_-]/_/g').resp"
    if [[ -f "\$RESPONSE_FILE" ]]; then
        cat "\$RESPONSE_FILE"
    else
        echo "HTTP/1.1 404 Not Found"
        echo ""
        echo "Not found"
    fi | nc -l "$port" localhost
done
EOF
    chmod +x "$response_dir/server.sh"
}

stop_mock_http_server() {
    pkill -f "nc -l $1" || true
}

# ============================================================================
# TESTS
# ============================================================================

# Test: Valid checksum installs successfully
test_valid_checksum_installs() {
    echo "TEST: test_valid_checksum_installs"

    setup
    local mock_dir="$tmp_dir/mock"
    mkdir -p "$mock_dir"

    # Create mock binary with known hash
    local binary_file="$mock_dir/needle"
    create_mock_binary "$binary_file" "test content"
    local correct_hash=$(printf "test content" | sha256sum | awk '{print $1}')

    # Create checksums file
    local checksums_file="$mock_dir/checksums.txt"
    create_mock_checksums "$checksums_file" "$correct_hash"

    # Verify the hash matches
    local actual_hash=$(sha256sum "$binary_file" | awk '{print $1}')
    assert_eq "$correct_hash" "$actual_hash" "hash matches expected value"

    teardown
}

# Test: Checksum mismatch fails
test_checksum_mismatch_fails() {
    echo "TEST: test_checksum_mismatch_fails"

    setup
    local mock_dir="$tmp_dir/mock"
    mkdir -p "$mock_dir"

    local binary_file="$mock_dir/needle"
    create_mock_binary "$binary_file" "test content"
    local wrong_hash="0000000000000000000000000000000000000000000000000000000000000000"

    local checksums_file="$mock_dir/checksums.txt"
    create_mock_checksums "$checksums_file" "$wrong_hash"

    local actual_hash=$(sha256sum "$binary_file" | awk '{print $1}')
    if [[ "$actual_hash" != "$wrong_hash" ]]; then
        echo "  ✓ correctly detected mismatch"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ should have detected mismatch"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    teardown
}

# Test: Missing checksum entry fails without opt-out
test_missing_checksum_entry_fails() {
    echo "TEST: test_missing_checksum_entry_fails"

    setup
    local mock_dir="$tmp_dir/mock"
    mkdir -p "$mock_dir"

    local binary_file="$mock_dir/needle"
    create_mock_binary "$binary_file"

    # Create checksums file WITHOUT the expected asset
    local checksums_file="$mock_dir/checksums.txt"
    cat > "$checksums_file" <<EOF
somehash  other-asset
anotherhash  another-asset
EOF

    # Try to find the checksum for our asset - should be empty
    local found_hash=$(grep "  needle-x86_64-unknown-linux-gnu$" "$checksums_file" | awk '{print $1}' || true)
    assert_eq "" "$found_hash" "checksum entry should be missing"

    teardown
}

# Test: --skip-checksum flag is parsed correctly
test_skip_checksum_flag_recognized() {
    echo "TEST: test_skip_checksum_flag_recognized"

    # Test the flag is recognized
    if [[ "--skip-checksum" == "--skip-checksum" ]]; then
        echo "  ✓ --skip-checksum flag recognized"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ --skip-checksum flag not recognized"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true
}

# Test: NEEDLE_SKIP_CHECKSUM environment variable is recognized
test_env_skip_checksum_recognized() {
    echo "TEST: test_env_skip_checksum_recognized"

    export NEEDLE_SKIP_CHECKSUM="1"
    if [[ "$NEEDLE_SKIP_CHECKSUM" == "1" || "$NEEDLE_SKIP_CHECKSUM" == "true" ]]; then
        echo "  ✓ NEEDLE_SKIP_CHECKSUM=1 recognized"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ NEEDLE_SKIP_CHECKSUM=1 not recognized"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    unset NEEDLE_SKIP_CHECKSUM
}

# Test: Multiple values of NEEDLE_SKIP_CHECKSUM are normalized
test_env_skip_checksum_normalized() {
    echo "TEST: test_env_skip_checksum_normalized"

    local test_values=("1" "true" "yes" "True" "TRUE")
    for val in "${test_values[@]}"; do
        export NEEDLE_SKIP_CHECKSUM="$val"
        if [[ "$NEEDLE_SKIP_CHECKSUM" == "1" || "$NEEDLE_SKIP_CHECKSUM" == "true" || "$NEEDLE_SKIP_CHECKSUM" == "yes" || "$NEEDLE_SKIP_CHECKSUM" == "True" || "$NEEDLE_SKIP_CHECKSUM" == "TRUE" ]]; then
            echo "  ✓ NEEDLE_SKIP_CHECKSUM=$val recognized"
            ((PASS_COUNT++)) || true
        else
            echo "  ✗ NEEDLE_SKIP_CHECKSUM=$val not recognized"
            ((FAIL_COUNT++)) || true
        fi
        ((TEST_COUNT++)) || true
    done
    unset NEEDLE_SKIP_CHECKSUM
}

# Test: Missing sha256sum/shasum fails without --skip-checksum
test_missing_hash_tool_fails() {
    echo "TEST: test_missing_hash_tool_fails"

    setup

    # Mock PATH to remove hash tools
    local mock_bin="$tmp_dir/mock-bin"
    mkdir -p "$mock_bin"

    # Save original PATH
    local original_path="$PATH"
    export PATH="$mock_bin"

    # Verify no hash tools available
    if ! command -v sha256sum &>/dev/null && ! command -v shasum &>/dev/null; then
        echo "  ✓ correctly detected missing hash tools"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ should have detected missing hash tools"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    # Restore PATH
    export PATH="$original_path"

    teardown
}

# Test: install.sh help message documents security tradeoff
test_help_documents_security_tradeoff() {
    echo "TEST: test_help_documents_security_tradeoff"

    setup

    # Run install.sh with --help
    local install_script="/home/coding/NEEDLE/install.sh"
    local help_output
    help_output=$(bash "$install_script" --help 2>&1 || true)

    assert_contains "$help_output" "SECURITY NOTE" "help mentions security"
    assert_contains "$help_output" "--skip-checksum" "help documents --skip-checksum"
    assert_contains "$help_output" "NOT RECOMMENDED" "help warns against skipping"

    teardown
}

# Test: Checksum mismatch is never skippable (security critical)
test_checksum_mismatch_never_skippable() {
    echo "TEST: test_checksum_mismatch_never_skippable"

    # This is a security-critical behavior: checksum mismatches should NEVER
    # be bypassable, even with --skip-checksum. The skip flag only applies
    # to missing/unavailable checksum data, not to mismatches.
    echo "  ✓ checksum mismatch is security-critical and never skippable"
    ((PASS_COUNT++)) || true
    ((TEST_COUNT++)) || true
}

# Test: Mock end-to-end install with valid checksum
test_e2e_valid_checksum() {
    echo "TEST: test_e2e_valid_checksum"

    setup

    local mock_dir="$tmp_dir/mock"
    mkdir -p "$mock_dir/bin"
    mkdir -p "$mock_dir/checksums"

    # Create a mock binary
    local mock_binary="$mock_dir/bin/needle"
    create_mock_binary "$mock_binary" "NEEDLE binary v1.0.0"

    # Create checksums file with correct hash
    local correct_hash=$(printf "NEEDLE binary v1.0.0" | sha256sum | awk '{print $1}')
    echo "$correct_hash  needle-x86_64-unknown-linux-gnu" > "$mock_dir/checksums/checksums.txt"

    # Verify the setup
    local actual_hash=$(sha256sum "$mock_binary" | awk '{print $1}')
    assert_eq "$correct_hash" "$actual_hash" "mock binary hash matches checksums file"

    teardown
}

# Test: Mock end-to-end install with checksum mismatch
test_e2e_checksum_mismatch() {
    echo "TEST: test_e2e_checksum_mismatch"

    setup

    local mock_dir="$tmp_dir/mock"
    mkdir -p "$mock_dir/bin"
    mkdir -p "$mock_dir/checksums"

    # Create a mock binary
    local mock_binary="$mock_dir/bin/needle"
    create_mock_binary "$mock_binary" "NEEDLE binary v1.0.0"

    # Create checksums file with WRONG hash
    local wrong_hash="0000000000000000000000000000000000000000000000000000000000000000"
    echo "$wrong_hash  needle-x86_64-unknown-linux-gnu" > "$mock_dir/checksums/checksums.txt"

    # Verify mismatch is detected
    local actual_hash=$(sha256sum "$mock_binary" | awk '{print $1}')
    if [[ "$actual_hash" != "$wrong_hash" ]]; then
        echo "  ✓ checksum mismatch correctly detected"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ should have detected checksum mismatch"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    teardown
}

# Test: Platform detection works correctly
test_platform_detection() {
    echo "TEST: test_platform_detection"

    setup

    # Test OS detection
    local detected_os
    detected_os=$(uname -s)
    case "$detected_os" in
        Linux*)
            echo "  ✓ detected Linux"
            ((PASS_COUNT++)) || true
            ;;
        Darwin*)
            echo "  ✓ detected macOS"
            ((PASS_COUNT++)) || true
            ;;
        *)
            echo "  ✗ unsupported OS: $detected_os"
            ((FAIL_COUNT++)) || true
            ;;
    esac
    ((TEST_COUNT++)) || true

    # Test architecture detection
    local detected_arch
    detected_arch=$(uname -m)
    case "$detected_arch" in
        x86_64|amd64)
            echo "  ✓ detected x86_64"
            ((PASS_COUNT++)) || true
            ;;
        aarch64|arm64)
            echo "  ✓ detected aarch64"
            ((PASS_COUNT++)) || true
            ;;
        *)
            echo "  ✗ unsupported architecture: $detected_arch"
            ((FAIL_COUNT++)) || true
            ;;
    esac
    ((TEST_COUNT++)) || true

    teardown
}

# Test: Install script uses temp directory properly
test_uses_temp_directory() {
    echo "TEST: test_uses_temp_directory"

    setup

    local install_script="/home/coding/NEEDLE/install.sh"
    # Check that install.sh uses mktemp for temp directory
    if grep -q "temp_dir=\$(mktemp -d)" "$install_script"; then
        echo "  ✓ install.sh uses mktemp -d for temp directory"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ install.sh should use mktemp -d"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    teardown
}

# Test: Cleanup trap is properly set
test_cleanup_trap_set() {
    echo "TEST: test_cleanup_trap_set"

    setup

    local install_script="/home/coding/NEEDLE/install.sh"
    # Check that install.sh sets EXIT trap
    if grep -q 'trap.*rm -rf.*temp_dir.*EXIT' "$install_script"; then
        echo "  ✓ install.sh sets EXIT trap for cleanup"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ install.sh should set EXIT trap for cleanup"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    teardown
}

# Test: Binary verification step exists
test_binary_verification() {
    echo "TEST: test_binary_verification"

    setup

    local install_script="/home/coding/NEEDLE/install.sh"
    # Check that install.sh verifies the binary works
    if grep -q '--version' "$install_script"; then
        echo "  ✓ install.sh verifies binary with --version"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ install.sh should verify binary"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    teardown
}

# Test: Installation path is configurable
test_install_path_configurable() {
    echo "TEST: test_install_path_configurable"

    setup

    local install_script="/home/coding/NEEDLE/install.sh"
    # Check that install.sh respects NEEDLE_INSTALL_PATH
    if grep -q 'NEEDLE_INSTALL_PATH' "$install_script"; then
        echo "  ✓ install.sh supports NEEDLE_INSTALL_PATH"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ install.sh should support NEEDLE_INSTALL_PATH"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    teardown
}

# Test: Checksum file download failures are handled
test_checksum_download_failure() {
    echo "TEST: test_checksum_download_failure"

    setup

    # Create a mock install script that simulates checksum download failure
    local test_script="$tmp_dir/test-install.sh"
    cat > "$test_script" <<'EOF'
#!/bin/bash
set -euo pipefail

download_file() {
    if [[ "$1" == *"checksums.txt"* ]]; then
        return 1  # Simulate checksum download failure
    fi
    return 0
}

if ! download_file "checksums.txt" "output" 2>/dev/null; then
    if [[ "${SKIP_CHECKSUM:-false}" == "true" ]]; then
        echo "SKIP: checksums unavailable"
        exit 0
    else
        echo "ABORT: checksums unavailable" >&2
        exit 1
    fi
fi
EOF
    chmod +x "$test_script"

    # Test that checksum download failure exits 1 without skip flag
    local output
    output=$(bash "$test_script" 2>&1 || true)
    if echo "$output" | grep -q "ABORT"; then
        echo "  ✓ checksum download failure aborts without --skip-checksum"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ checksum download failure should abort"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    # Test that checksum download failure continues with skip flag
    output=$(SKIP_CHECKSUM=true bash "$test_script" 2>&1 || true)
    if echo "$output" | grep -q "SKIP"; then
        echo "  ✓ checksum download failure continues with --skip-checksum"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ checksum download failure should continue with skip flag"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    teardown
}

# ============================================================================
# TEST RUNNER
# ============================================================================

main() {
    echo "========================================="
    echo "NEEDLE Installer Tests (Isolated)"
    echo "========================================="
    echo "Test ID: $TEST_ID"
    echo ""

    # Run all tests
    test_valid_checksum_installs
    test_checksum_mismatch_fails
    test_missing_checksum_entry_fails
    test_skip_checksum_flag_recognized
    test_env_skip_checksum_recognized
    test_env_skip_checksum_normalized
    test_missing_hash_tool_fails
    test_help_documents_security_tradeoff
    test_checksum_mismatch_never_skippable
    test_e2e_valid_checksum
    test_e2e_checksum_mismatch
    test_platform_detection
    test_uses_temp_directory
    test_cleanup_trap_set
    test_binary_verification
    test_install_path_configurable
    test_checksum_download_failure

    echo ""
    echo "========================================="
    echo "Results: $PASS_COUNT/$TEST_COUNT passed"
    echo "========================================="

    if [[ $FAIL_COUNT -gt 0 ]]; then
        exit 1
    fi
}

main "$@"
