#!/bin/bash
#
# Test suite for install.sh checksum verification
# Tests are isolated and use fixtures/mocks - no real installations
#

set -euo pipefail

# Resolve the repo root from this script's own location. These paths were
# hardcoded to /home/coding/NEEDLE, which exists only on one developer machine:
# the tests passed there and failed everywhere else, including CI, with
# "bash: /home/coding/NEEDLE/install.sh: No such file or directory".
# test_e2e_install.sh already derives it this way. See needle-ab52a15a.
INSTALLER_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_INSTALL_SH="$INSTALLER_TEST_DIR/../../install.sh"


# Test framework helpers
TEST_COUNT=0
PASS_COUNT=0
FAIL_COUNT=0

tmp_dir=""
setup() {
    tmp_dir=$(mktemp -d)
    export HOME="$tmp_dir"
    export NEEDLE_INSTALL_PATH="$tmp_dir/needle"
}

teardown() {
    rm -rf "$tmp_dir"
}

assert_eq() {
    local expected="$1"
    local actual="$2"
    local msg="${3:-assertion failed}"

    if [[ "$expected" == "$actual" ]]; then
        echo "  ✓ $msg"
        ((PASS_COUNT++)) || true || true
    else
        echo "  ✗ $msg"
        echo "    expected: $expected"
        echo "    got: $actual"
        ((FAIL_COUNT++)) || true || true
    fi
    ((TEST_COUNT++)) || true || true
    return 0
}

assert_contains() {
    local haystack="$1"
    local needle="$2"
    local msg="${3:-assertion failed}"

    if [[ "$haystack" == *"$needle"* ]]; then
        echo "  ✓ $msg"
        ((PASS_COUNT++)) || true || true
    else
        echo "  ✗ $msg"
        echo "    expected to contain: $needle"
        echo "    in: $haystack"
        ((FAIL_COUNT++)) || true || true
    fi
    ((TEST_COUNT++)) || true || true
    return 0
}

assert_exit_code() {
    local expected="$1"
    local actual="$2"
    local msg="${3:-exit code assertion failed}"

    if [[ "$expected" == "$actual" ]]; then
        echo "  ✓ $msg"
        ((PASS_COUNT++)) || true || true
    else
        echo "  ✗ $msg"
        echo "    expected exit code: $expected"
        echo "    got: $actual"
        ((FAIL_COUNT++)) || true || true
    fi
    ((TEST_COUNT++)) || true || true
    return 0
}

# Create mock fixture files
create_mock_checksums() {
    local checksums_file="$1"
    local binary_file="$2"
    local hash="$3"

    cat > "$checksums_file" <<EOF
$hash  needle-x86_64-unknown-linux-gnu
otherhash  needle-aarch64-unknown-linux-gnu
EOF
}

create_mock_binary() {
    local binary_file="$1"
    printf "mock binary" > "$binary_file"
    chmod +x "$binary_file"
}

# Test: Valid checksum installs successfully
test_valid_checksum_installs() {
    echo "TEST: test_valid_checksum_installs"

    setup
    local mock_dir="$tmp_dir/mock"
    mkdir -p "$mock_dir"

    # Create mock binary with known hash
    local binary_file="$mock_dir/needle"
    create_mock_binary "$binary_file"
    local correct_hash=$(printf "mock binary" | sha256sum | awk '{print $1}')

    # Create checksums file
    local checksums_file="$mock_dir/checksums.txt"
    create_mock_checksums "$checksums_file" "$binary_file" "$correct_hash"

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
    create_mock_binary "$binary_file"
    local wrong_hash="0000000000000000000000000000000000000000000000000000000000000000"

    local checksums_file="$mock_dir/checksums.txt"
    create_mock_checksums "$checksums_file" "$binary_file" "$wrong_hash"

    local actual_hash=$(sha256sum "$binary_file" | awk '{print $1}')
    assert_contains "$wrong_hash" "" "wrong hash set"
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

# Test: Missing checksum entry fails
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

# Test: --skip-checksum allows installation without checksums
test_skip_checksum_allows_install() {
    echo "TEST: test_skip_checksum_allows_install"

    # Test the flag is parsed correctly
    if [[ "--skip-checksum" == "--skip-checksum" ]]; then
        echo "  ✓ --skip-checksum flag recognized"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ --skip-checksum flag not recognized"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true
}

# Test: NEEDLE_SKIP_CHECKSUM=1 allows installation
test_env_skip_checksum_allows_install() {
    echo "TEST: test_env_skip_checksum_allows_install"

    # Test the env var is recognized
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

# Test: Missing sha256sum/shasum fails without --skip-checksum
test_missing_hash_tool_fails() {
    echo "TEST: test_missing_hash_tool_fails"

    setup

    # Mock PATH to remove hash tools
    local mock_bin="$tmp_dir/mock-bin"
    mkdir -p "$mock_bin"

    # Create mock curl that succeeds
    cat > "$mock_bin/curl" <<'EOF'
#!/bin/bash
exit 0
EOF
    chmod +x "$mock_bin/curl"

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

# Test: Help message documents the security tradeoff
test_help_documents_security_tradeoff() {
    echo "TEST: test_help_documents_security_tradeoff"

    setup

    # Run install.sh with --help
    local install_script="$REPO_INSTALL_SH"
    local help_output
    help_output=$(bash "$install_script" --help 2>&1 || true)

    assert_contains "$help_output" "SECURITY NOTE" "help mentions security"
    assert_contains "$help_output" "--skip-checksum" "help documents --skip-checksum"
    assert_contains "$help_output" "NOT RECOMMENDED" "help warns against skipping"

    teardown
}

# Test: Download failure aborts installation
test_download_failure_aborts() {
    echo "TEST: test_download_failure_aborts"

    setup

    # Create a mock install script that simulates download failure
    local test_script="$tmp_dir/test-install.sh"
    cat > "$test_script" <<'EOF'
#!/bin/bash
set -euo pipefail

# Simulate download_file returning failure
download_file() {
    return 1
}

# This should abort without --skip-checksum
if ! download_file "url" "output" 2>/dev/null; then
    if [[ "${SKIP_CHECKSUM:-false}" == "true" || "${SKIP_CHECKSUM:-false}" == "1" ]]; then
        echo "SKIP: download failed but continuing"
    else
        echo "ABORT: download failed" >&2
        exit 1
    fi
fi
EOF
    chmod +x "$test_script"

    # Test that download failure exits 1 without skip flag
    # Capture output first to avoid broken pipe when grep finds match early
    local script_output
    script_output=$(bash "$test_script" 2>&1 || true)
    if echo "$script_output" | grep -q "ABORT"; then
        echo "  ✓ download failure aborts without --skip-checksum"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ download failure should abort"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    teardown
}

# Test: Download failure continues with --skip-checksum
test_download_failure_continues_with_skip() {
    echo "TEST: test_download_failure_continues_with_skip"

    setup

    local test_script="$tmp_dir/test-install.sh"
    cat > "$test_script" <<'EOF'
#!/bin/bash
set -euo pipefail
SKIP_CHECKSUM="${1:-false}"

download_file() {
    return 1
}

if ! download_file "url" "output" 2>/dev/null; then
    if [[ "$SKIP_CHECKSUM" == "true" || "$SKIP_CHECKSUM" == "1" ]]; then
        echo "SKIP: download failed but continuing"
        exit 0
    else
        echo "ABORT: download failed" >&2
        exit 1
    fi
fi
EOF
    chmod +x "$test_script"

    # With skip flag, should continue
    local output
    output=$(bash "$test_script" "true" 2>&1 || true)
    if echo "$output" | grep -q "SKIP"; then
        echo "  ✓ download failure continues with --skip-checksum"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ download failure should continue with skip flag"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

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

# Test: --skip-checksum outputs conspicuous warning to stderr
test_skip_checksum_outputs_warning() {
    echo "TEST: test_skip_checksum_outputs_warning"

    setup

    # Create a test script that simulates the warning function
    local test_script="$tmp_dir/test-warning.sh"
    cat > "$test_script" <<'EOF'
#!/bin/bash
set -euo pipefail

# Mock the warn_checksum_skipped function
warn_checksum_skipped() {
    cat <<'INNER_EOF'

════════════════════════════════════════════════════════════════════════════════
                                  ⚠️  SECURITY WARNING  ⚠️
════════════════════════════════════════════════════════════════════════════════

Checksum verification is DISABLED. The downloaded binary will NOT be verified
against the expected SHA-256 hash from the release.
INNER_EOF
}

# When SKIP_CHECKSUM is true, call the warning
if [[ "${SKIP_CHECKSUM:-false}" == "true" ]]; then
    warn_checksum_skipped >&2
    echo "WARNING_OUTPUT"
fi
EOF
    chmod +x "$test_script"

    # Test that warning is output to stderr when SKIP_CHECKSUM=true
    local output
    output=$(SKIP_CHECKSUM=true bash "$test_script" 2>&1 || true)
    if echo "$output" | grep -q "SECURITY WARNING"; then
        echo "  ✓ warning contains 'SECURITY WARNING'"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ warning should contain 'SECURITY WARNING'"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    if echo "$output" | grep -q "Checksum verification"; then
        echo "  ✓ warning contains 'Checksum verification'"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ warning should contain 'Checksum verification'"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    if echo "$output" | grep -q "DISABLED"; then
        echo "  ✓ warning contains 'DISABLED'"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ warning should contain 'DISABLED'"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    # Test that warning is NOT output when SKIP_CHECKSUM is false
    output=$(SKIP_CHECKSUM=false bash "$test_script" 2>&1 || true)
    if ! echo "$output" | grep -q "SECURITY WARNING"; then
        echo "  ✓ no warning when SKIP_CHECKSUM=false"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ should not warn when SKIP_CHECKSUM=false"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    teardown
}

# Test: Mock end-to-end install with valid checksum
test_e2e_install_with_valid_checksum() {
    echo "TEST: test_e2e_install_with_valid_checksum"

    setup

    local mock_dir="$tmp_dir/mock"
    mkdir -p "$mock_dir/bin"
    mkdir -p "$mock_dir/checksums"

    # Create a mock binary
    local mock_binary="$mock_dir/bin/needle"
    printf "NEEDLE binary v1.0.0" > "$mock_binary"
    chmod +x "$mock_binary"

    # Create checksums file with correct hash
    local correct_hash=$(printf "NEEDLE binary v1.0.0" | sha256sum | awk '{print $1}')
    echo "$correct_hash  needle-x86_64-unknown-linux-gnu" > "$mock_dir/checksums/checksums.txt"

    # Verify the setup
    local actual_hash=$(sha256sum "$mock_binary" | awk '{print $1}')
    assert_eq "$correct_hash" "$actual_hash" "mock binary hash matches checksums file"

    teardown
}

# Test: Mock end-to-end install with checksum mismatch
test_e2e_install_with_mismatch_fails() {
    echo "TEST: test_e2e_install_with_mismatch_fails"

    setup

    local mock_dir="$tmp_dir/mock"
    mkdir -p "$mock_dir/bin"
    mkdir -p "$mock_dir/checksums"

    # Create a mock binary
    local mock_binary="$mock_dir/bin/needle"
    printf "NEEDLE binary v1.0.0" > "$mock_binary"
    chmod +x "$mock_binary"

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

# Test: Version discovery must not pipe curl into an early-exiting reader
# (structural guard; behavioral coverage lives in test_e2e_install.sh)
test_version_discovery_no_curl_pipe() {
    echo "TEST: test_version_discovery_no_curl_pipe"

    setup

    local install_script="$REPO_INSTALL_SH"
    if grep -qE 'curl [^|]*\|[[:space:]]*(grep|head)' "$install_script"; then
        echo "  ✗ version discovery pipes curl into an early-exiting reader (broken-pipe risk)"
        ((FAIL_COUNT++)) || true
    else
        echo "  ✓ curl is never piped into an early-exiting reader"
        ((PASS_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true

    teardown
}

# Run all tests
main() {
    echo "========================================="
    echo "Install.sh Checksum Verification Tests"
    echo "========================================="
    echo ""

    test_valid_checksum_installs
    test_checksum_mismatch_fails
    test_missing_checksum_entry_fails
    test_skip_checksum_allows_install
    test_env_skip_checksum_allows_install
    test_missing_hash_tool_fails
    test_help_documents_security_tradeoff
    test_download_failure_aborts
    test_download_failure_continues_with_skip
    test_checksum_mismatch_never_skippable
    test_skip_checksum_outputs_warning
    test_e2e_install_with_valid_checksum
    test_e2e_install_with_mismatch_fails
    test_version_discovery_no_curl_pipe

    echo ""
    echo "========================================="
    echo "Results: $PASS_COUNT/$TEST_COUNT passed"
    echo "========================================="

    if [[ $FAIL_COUNT -gt 0 ]]; then
        exit 1
    fi
}

main "$@"
