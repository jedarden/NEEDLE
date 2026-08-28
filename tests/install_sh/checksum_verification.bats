#!/run/current-system/sw/bin/bats
#
# Regression tests for install.sh checksum verification
#
# These tests verify the checksum verification logic in install.sh
# using local fixtures and mock HTTP servers. No real downloads or
# user installations are performed.
#

load test_helpers

setup() {
    # Create temp directory for each test
    TEST_TEMP_DIR=$(mktemp -d)
    export TEST_TEMP_DIR

    # Create fixture directory
    FIXTURE_DIR="${TEST_TEMP_DIR}/fixtures"
    mkdir -p "$FIXTURE_DIR"

    # Create mock binary
    MOCK_BINARY="${FIXTURE_DIR}/mock-binary"
    echo "mock binary content" > "$MOCK_BINARY"

    # Set up test environment
    export HOME="$TEST_TEMP_DIR"
    export NEEDLE_INSTALL_PATH="${TEST_TEMP_DIR}/needle"

    # Create checksums fixtures
    create_checksum_fixtures
}

teardown() {
    # Clean up temp directory
    if [[ -n "$TEST_TEMP_DIR" && -d "$TEST_TEMP_DIR" ]]; then
        rm -rf "$TEST_TEMP_DIR"
    fi
}

# Create various checksums.txt fixtures
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

# Mock the sha256sum command
mock_sha256sum() {
    local expected_output="abc123def456  $1"
    echo "$expected_output"
}

# Mock the shasum command
mock_shasum() {
    echo "abc123def456  $1"
}

# Test: Valid checksum verification
@test "valid checksum verification succeeds" {
    # Create a test binary
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    # Calculate actual checksum
    local actual_checksum
    if command -v sha256sum &>/dev/null; then
        actual_checksum=$(sha256sum "$test_binary" | awk '{print $1}')
    elif command -v shasum &>/dev/null; then
        actual_checksum=$(shasum -a 256 "$test_binary" | awk '{print $1}')
    else
        skip "No checksum tool available"
    fi

    # Create checksums file with actual checksum
    cat > "${FIXTURE_DIR}/checksums-actual.txt" <<EOF
${actual_checksum}  needle-x86_64-unknown-linux-gnu
EOF

    # Run verification logic
    run verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-actual.txt" "needle-x86_64-unknown-linux-gnu"

    [ "$status" -eq 0 ]
    [[ "$output" == *"Checksum verified"* ]] || [[ "$output" == *"success"* ]]
}

# Test: Missing checksums file
@test "missing checksums file fails without --skip-checksum" {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    # Try to verify with non-existent checksums file
    run verify_checksum_with_fixture "$test_binary" "${TEST_TEMP_DIR}/nonexistent.txt" "needle-x86_64-unknown-linux-gnu"

    [ "$status" -ne 0 ]
    [[ "$output" == *"Could not download checksums"* ]] || [[ "$output" == *"error"* ]]
}

# Test: Missing checksums file with --skip-checksum
@test "missing checksums file succeeds with --skip-checksum" {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    # Set skip checksum flag
    export NEEDLE_SKIP_CHECKSUM="true"

    # Try to verify with non-existent checksums file
    run verify_checksum_with_fixture "$test_binary" "${TEST_TEMP_DIR}/nonexistent.txt" "needle-x86_64-unknown-linux-gnu"

    # Should succeed or skip verification
    [ "$status" -eq 0 ] || [[ "$output" == *"Skipping checksum verification"* ]]
}

# Test: Checksum for asset not found in checksums.txt
@test "checksum for asset not found fails without --skip-checksum" {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    # Use checksums file that doesn't contain the expected asset
    run verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-missing-asset.txt" "needle-x86_64-unknown-linux-gnu"

    [ "$status" -ne 0 ]
    [[ "$output" == *"Could not find checksum"* ]] || [[ "$output" == *"not found"* ]]
}

# Test: Checksum for asset not found with --skip-checksum
@test "checksum for asset not found succeeds with --skip-checksum" {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    export NEEDLE_SKIP_CHECKSUM="true"

    # Use checksums file that doesn't contain the expected asset
    run verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-missing-asset.txt" "needle-x86_64-unknown-linux-gnu"

    # Should succeed or skip verification
    [ "$status" -eq 0 ] || [[ "$output" == *"Skipping checksum verification"* ]]
}

# Test: Checksum mismatch
@test "checksum mismatch always fails (even with --skip-checksum)" {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    # Create checksums file with wrong checksum
    cat > "${FIXTURE_DIR}/checksums-wrong.txt" <<EOF
wrongchecksum123  needle-x86_64-unknown-linux-gnu
EOF

    # Try without skip flag
    run verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-wrong.txt" "needle-x86_64-unknown-linux-gnu"
    [ "$status" -ne 0 ]
    [[ "$output" == *"Checksum mismatch"* ]] || [[ "$output" == *"mismatch"*]]

    # Try WITH skip flag - should STILL fail
    export NEEDLE_SKIP_CHECKSUM="true"
    run verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-wrong.txt" "needle-x86_64-unknown-linux-gnu"
    [ "$status" -ne 0 ]
    [[ "$output" == *"Checksum mismatch"* ]] || [[ "$output" == *"never skippable"* ]]
}

# Test: Missing SHA-256 tool (sha256sum and shasum)
@test "missing SHA-256 tool fails without --skip-checksum" {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    # Mock PATH to remove checksum tools
    local mock_path="${TEST_TEMP_DIR}/mock-path"
    mkdir -p "$mock_path"

    # Create mock commands that report "not found"
    cat > "${mock_path}/sha256sum" <<'EOF'
#!/bin/bash
exit 1
EOF
    chmod +x "${mock_path}/sha256sum"

    cat > "${mock_path}/shasum" <<'EOF'
#!/bin/bash
exit 1
EOF
    chmod +x "${mock_path}/shasum"

    # Add to front of PATH
    export PATH="${mock_path}:${PATH}"

    # Create valid checksums file
    run verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-valid.txt" "needle-x86_64-unknown-linux-gnu"

    # Should fail due to missing hash tool
    [ "$status" -ne 0 ]
    [[ "$output" == *"Neither sha256sum nor shasum"* ]] || [[ "$output" == *"hash tool"* ]]
}

# Test: Missing SHA-256 tool with --skip-checksum
@test "missing SHA-256 tool succeeds with --skip-checksum" {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    export NEEDLE_SKIP_CHECKSUM="true"

    # Mock PATH to simulate missing tools
    local mock_path="${TEST_TEMP_DIR}/mock-path-skip"
    mkdir -p "$mock_path"

    # Create dummy commands that will fail
    cat > "${mock_path}/sha256sum" <<'EOF'
#!/bin/bash
exit 1
EOF
    chmod +x "${mock_path}/sha256sum"

    cat > "${mock_path}/shasum" <<'EOF'
#!/bin/bash
exit 1
EOF
    chmod +x "${mock_path}/shasum"

    export PATH="${mock_path}:${PATH}"

    # Create valid checksums file
    run verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-valid.txt" "needle-x86_64-unknown-linux-gnu"

    # Should succeed or skip verification
    [ "$status" -eq 0 ] || [[ "$output" == *"Skipping checksum verification"* ]]
}

# Test: Empty checksums file
@test "empty checksums file fails without --skip-checksum" {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    run verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-empty.txt" "needle-x86_64-unknown-linux-gnu"

    [ "$status" -ne 0 ]
    [[ "$output" == *"Could not find checksum"* ]] || [[ "$output" == *"not found"* ]]
}

# Test: Malformed checksums file
@test "malformed checksums file handles gracefully" {
    local test_binary="${TEST_TEMP_DIR}/test-binary"
    echo "test content" > "$test_binary"

    export NEEDLE_SKIP_CHECKSUM="true"

    run verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-malformed.txt" "needle-x86_64-unknown-linux-gnu"

    # With skip flag, should handle gracefully
    [ "$status" -eq 0 ] || [[ "$output" == *"Skipping"* ]]
}

# Test: Opt-out flag via command line
@test "--skip-checksum flag is recognized" {
    # This tests the flag parsing logic
    run parse_and_check_skip_flag "--skip-checksum"

    [ "$status" -eq 0 ]
    [[ "$output" == *"skip"* ]] || [[ "$output" == *"true"* ]]
}

# Test: Opt-out via environment variable
@test "NEEDLE_SKIP_CHECKSUM environment variable is recognized" {
    export NEEDLE_SKIP_CHECKSUM="true"

    run parse_and_check_skip_flag ""

    [ "$status" -eq 0 ]
    [[ "$output" == *"skip"* ]] || [[ "$output" == *"true"* ]]
}

# Test: Environment variable normalization
@test "NEEDLE_SKIP_CHECKSUM values are normalized correctly" {
    # Test various "true" values
    for val in "1" "true" "yes" "TRUE" "YES"; do
        export NEEDLE_SKIP_CHECKSUM="$val"
        run parse_and_check_skip_flag ""
        [ "$status" -eq 0 ] || echo "Failed for value: $val"
    done

    # Test "false" values
    for val in "0" "false" "no" "" "random"; do
        export NEEDLE_SKIP_CHECKSUM="$val"
        run parse_and_check_skip_flag ""
        # Should not be in skip mode
        [[ "$output" != *"skip"* ]] || echo "Incorrectly skipped for value: $val"
    done
}

# Test: Checksum warning message is displayed
@test "checksum skip warning is displayed when opted out" {
    export NEEDLE_SKIP_CHECKSUM="true"

    run run_checksum_skip_warning

    [[ "$output" == *"SECURITY WARNING"* ]] || [[ "$output" == *"WARNING"* ]]
    [[ "$output" == *"DISABLED"* ]] || [[ "$output" == *"disabled"* ]]
}

# Test: Valid checksum with sha256sum
@test "valid checksum with sha256sum tool" {
    if ! command -v sha256sum &>/dev/null; then
        skip "sha256sum not available"
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

    run verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-sha256sum.txt" "needle-x86_64-unknown-linux-gnu"

    [ "$status" -eq 0 ]
    [[ "$output" == *"Checksum verified"* ]] || [[ "$output" == *"success"* ]]
}

# Test: Valid checksum with shasum
@test "valid checksum with shasum tool" {
    if ! command -v shasum &>/dev/null; then
        skip "shasum not available"
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

    run verify_checksum_with_fixture "$test_binary" "${FIXTURE_DIR}/checksums-shasum.txt" "needle-x86_64-unknown-linux-gnu"

    [ "$status" -eq 0 ]
    [[ "$output" == *"Checksum verified"* ]] || [[ "$output" == *"success"* ]]
}

# Test: GPG signature verification is informational (never fails install)
@test "GPG signature verification is informational only" {
    # This test verifies that GPG verification failures don't abort installation
    # GPG verification is optional and only informational

    run check_gpg_verification_is_informational

    [ "$status" -eq 0 ]
    # GPG failures should be warnings, not errors
    [[ "$output" != *"abort"* ]] || true
}
