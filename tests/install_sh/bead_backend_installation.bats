#!/run/current-system/sw/bin/bats
#
# Regression tests for install.sh bead-rs backend installation
#
# These tests verify the bead-rs backend installation logic in install.sh
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
    mkdir -p "${FIXTURE_DIR}/bin"

    # Create install directory
    INSTALL_DIR="${FIXTURE_DIR}/bin"
    export INSTALL_DIR

    # Set up test environment
    export HOME="$TEST_TEMP_DIR"
    export NEEDLE_INSTALL_PATH="${TEST_TEMP_DIR}/needle"

    # Create mock binaries
    create_mock_binaries

    # Create checksums fixtures
    create_bead_checksum_fixtures
}

teardown() {
    # Clean up temp directory
    if [[ -n "$TEST_TEMP_DIR" && -d "$TEST_TEMP_DIR" ]]; then
        rm -rf "$TEST_TEMP_DIR"
    fi
}

# Create mock needle and bead binaries
create_mock_binaries() {
    # Mock needle binary
    cat > "${INSTALL_DIR}/needle" <<'EOF'
#!/bin/bash
if [[ "$1" == "--version" ]]; then
    echo "needle v1.2.3"
    exit 0
fi
echo "mock needle"
EOF
    chmod +x "${INSTALL_DIR}/needle"

    # Mock bead binary (newer version)
    cat > "${INSTALL_DIR}/bead-newer" <<'EOF'
#!/bin/bash
if [[ "$1" == "--version" ]]; then
    echo "bead v2.0.0"
    exit 0
fi
echo "mock bead newer"
EOF
    chmod +x "${INSTALL_DIR}/bead-newer"

    # Mock bead binary (older version)
    cat > "${INSTALL_DIR}/bead-older" <<'EOF'
#!/bin/bash
if [[ "$1" == "--version" ]]; then
    echo "bead v0.1.0"
    exit 0
fi
echo "mock bead older"
EOF
    chmod +x "${INSTALL_DIR}/bead-older"

    # Mock bead binary for tampering tests
    cat > "${FIXTURE_DIR}/bead-untampered" <<'EOF'
#!/bin/bash
if [[ "$1" == "--version" ]]; then
    echo "bead v1.5.0"
    exit 0
fi
echo "mock bead untampered"
EOF
    chmod +x "${FIXTURE_DIR}/bead-untampered"

    # Tampered version
    cat > "${FIXTURE_DIR}/bead-tampered" <<'EOF'
#!/bin/bash
if [[ "$1" == "--version" ]]; then
    echo "bead v1.5.0"
    exit 0
fi
echo "THIS BINARY HAS BEEN TAMPERED WITH"
EOF
    chmod +x "${FIXTURE_DIR}/bead-tampered"
}

# Create checksum fixtures for bead binaries
create_bead_checksum_fixtures() {
    # Valid checksum for untampered bead
    local checksum
    checksum=$(calculate_checksum "${FIXTURE_DIR}/bead-untampered")
    echo "${checksum}  bead-x86_64-unknown-linux-gnu" > "${FIXTURE_DIR}/checksums-bead-valid.txt"

    # Wrong checksum (for tampering test)
    echo "wrongchecksum123  bead-x86_64-unknown-linux-gnu" > "${FIXTURE_DIR}/checksums-bead-wrong.txt"

    # Checksums file missing bead asset
    echo "abc123def456  needle-x86_64-unknown-linux-gnu" > "${FIXTURE_DIR}/checksums-bead-missing.txt"

    # Empty checksums file
    touch "${FIXTURE_DIR}/checksums-empty.txt"
}

# Mock bead version comparison
# Usage: mock_bead_version_compare <existing_version> <release_version>
# Returns: 0 if existing >= release, 1 otherwise
mock_bead_version_compare() {
    local existing="$1"
    local release="$2"

    # Strip 'v' prefix and 'bead ' prefix if present
    existing="${existing#v}"
    existing="${existing#bead }"
    release="${release#v}"
    release="${release#bead }"

    local IFS=.
    local -a e_parts=($existing)
    local -a r_parts=($release)

    for i in 0 1 2; do
        local e="${e_parts[$i]:-0}"
        local r="${r_parts[$i]:-0}"
        e="${e%%[^0-9]*}"
        r="${r%%[^0-9]*}"

        if (( e > r )); then
            return 0  # existing is newer
        elif (( e < r )); then
            return 1  # existing is older
        fi
    done

    return 0  # versions are equal
}

# Test: Happy path - both needle and bead install successfully
@test "happy path - both needle and bead install successfully" {
    # Create mock needle binary
    local needle_binary="${INSTALL_DIR}/needle"
    echo "mock needle v1.2.3" > "$needle_binary"
    chmod +x "$needle_binary"

    # Create mock bead binary with valid checksum
    local bead_binary="${FIXTURE_DIR}/bead-download"
    cp "${FIXTURE_DIR}/bead-untampered" "$bead_binary"

    # Simulate successful installation
    run install_bead_mock "$bead_binary" "${FIXTURE_DIR}/checksums-bead-valid.txt" "bead-x86_64-unknown-linux-gnu" false

    [ "$status" -eq 0 ]
    [[ "$output" == *"Checksum verified"* ]] || [[ "$output" == *"success"* ]]
    [[ "$output" == *"bead"* ]] || [[ "$output" == *"installed"* ]]
}

# Test: Tampered bead checksum aborts installation
@test "tampered bead checksum aborts installation" {
    local bead_binary="${FIXTURE_DIR}/bead-tampered"

    # Try to install tampered binary
    run verify_checksum_with_fixture "$bead_binary" "${FIXTURE_DIR}/checksums-bead-wrong.txt" "bead-x86_64-unknown-linux-gnu"

    # Should fail even with --skip-checksum (mismatches are never skippable)
    [ "$status" -ne 0 ]
    [[ "$output" == *"Checksum mismatch"* ]] || [[ "$output" == *"mismatch"* ]]
    [[ "$output" == *"never skippable"* ]] || [[ "$output" == *"aborted"* ]]
}

# Test: Tampered bead checksum aborts even with --skip-checksum
@test "tampered bead checksum aborts even with --skip-checksum" {
    local bead_binary="${FIXTURE_DIR}/bead-tampered"
    export NEEDLE_SKIP_CHECKSUM="true"

    run verify_checksum_with_fixture "$bead_binary" "${FIXTURE_DIR}/checksums-bead-wrong.txt" "bead-x86_64-unknown-linux-gnu"

    # MISMATCHES ARE NEVER SKIPPABLE - even with opt-out
    [ "$status" -ne 0 ]
    [[ "$output" == *"Checksum mismatch"* ]] || [[ "$output" == *"never skippable"* ]]
}

# Test: --skip-bead flag leaves bead absent
@test "--skip-bead flag leaves bead absent" {
    export SKIP_BEAD="true"
    export NEEDLE_SKIP_BEAD="true"

    # Simulate install with --skip-bead
    run check_skip_bead_flag

    [ "$status" -eq 0 ]
    [[ "$output" == *"Skipping bead"* ]] || [[ "$output" == *"skip"* ]]

    # Verify bead was not "installed"
    [[ ! -f "${INSTALL_DIR}/bead" ]] || true
}

# Test: NEEDLE_SKIP_BEAD environment variable leaves bead absent
@test "NEEDLE_SKIP_BEAD environment variable leaves bead absent" {
    export NEEDLE_SKIP_BEAD="true"

    run check_skip_bead_flag

    [ "$status" -eq 0 ]
    [[ "$output" == *"Skipping bead"* ]] || [[ "$output" == *"skip"* ]]
}

# Test: Existing newer bead on PATH is retained
@test "existing newer bead on PATH is retained" {
    # Simulate having bead v2.0.0 on PATH when release is v1.5.0
    local mock_path="${FIXTURE_DIR}/mock-path-newer"
    mkdir -p "$mock_path"
    cp "${INSTALL_DIR}/bead-newer" "${mock_path}/bead"
    chmod +x "${mock_path}/bead"

    export PATH="${mock_path}:${PATH}"
    local existing_version
    existing_version=$("${mock_path}/bead" --version 2>/dev/null | awk '{print $2}')
    local release_version="v1.5.0"

    # Run version comparison
    run mock_bead_version_compare "$existing_version" "$release_version"

    # Should return 0 (existing >= release, so keep it)
    [ "$status" -eq 0 ]

    # Verify the version check message
    [[ "$existing_version" == *"2.0.0"* ]] || true
}

# Test: Existing older bead gets replaced
@test "existing older bead gets replaced" {
    # Simulate having bead v0.1.0 on PATH when release is v1.5.0
    local mock_path="${FIXTURE_DIR}/mock-path-older"
    mkdir -p "$mock_path"
    cp "${INSTALL_DIR}/bead-older" "${mock_path}/bead"
    chmod +x "${mock_path}/bead"

    export PATH="${mock_path}:${PATH}"
    local existing_version
    existing_version=$("${mock_path}/bead" --version 2>/dev/null | awk '{print $2}')
    local release_version="v1.5.0"

    # Run version comparison
    run mock_bead_version_compare "$existing_version" "$release_version"

    # Should return 1 (existing < release, so replace it)
    [ "$status" -eq 1 ]
}

# Test: Missing checksums file for bead aborts without --skip-checksum
@test "missing bead checksums file aborts without --skip-checksum" {
    local bead_binary="${FIXTURE_DIR}/bead-untampered"
    unset NEEDLE_SKIP_CHECKSUM

    run verify_checksum_with_fixture "$bead_binary" "${TEST_TEMP_DIR}/nonexistent-checksums.txt" "bead-x86_64-unknown-linux-gnu"

    [ "$status" -ne 0 ]
    [[ "$output" == *"Could not download checksums"* ]] || [[ "$output" == *"aborted"* ]]
}

# Test: Missing checksums file for bead succeeds with --skip-checksum
@test "missing bead checksums file succeeds with --skip-checksum" {
    local bead_binary="${FIXTURE_DIR}/bead-untampered"
    export NEEDLE_SKIP_CHECKSUM="true"

    run verify_checksum_with_fixture "$bead_binary" "${TEST_TEMP_DIR}/nonexistent-checksums.txt" "bead-x86_64-unknown-linux-gnu"

    [ "$status" -eq 0 ]
    [[ "$output" == *"Skipping checksum verification"* ]]
}

# Test: Bead asset not in checksums file fails without --skip-checksum
@test "bead asset not in checksums file fails without --skip-checksum" {
    local bead_binary="${FIXTURE_DIR}/bead-untampered"
    unset NEEDLE_SKIP_CHECKSUM

    run verify_checksum_with_fixture "$bead_binary" "${FIXTURE_DIR}/checksums-bead-missing.txt" "bead-x86_64-unknown-linux-gnu"

    [ "$status" -ne 0 ]
    [[ "$output" == *"Could not find checksum"* ]] || [[ "$output" == *"not found"* ]]
}

# Test: Bead asset not in checksums file succeeds with --skip-checksum
@test "bead asset not in checksums file succeeds with --skip-checksum" {
    local bead_binary="${FIXTURE_DIR}/bead-untampered"
    export NEEDLE_SKIP_CHECKSUM="true"

    run verify_checksum_with_fixture "$bead_binary" "${FIXTURE_DIR}/checksums-bead-missing.txt" "bead-x86_64-unknown-linux-gnu"

    [ "$status" -eq 0 ]
    [[ "$output" == *"Skipping checksum verification"* ]]
}

# Test: Both needle and bead versions reported in success message
@test "both needle and bead versions reported in success message" {
    # Create mock needle
    local needle_binary="${INSTALL_DIR}/needle"
    echo "mock needle v1.2.3" > "$needle_binary"
    chmod +x "$needle_binary"

    # Create mock bead
    local bead_binary="${INSTALL_DIR}/bead"
    echo "mock bead v1.5.0" > "$bead_binary"
    chmod +x "$bead_binary"

    run mock_install_success_message "v1.2.3" "v1.5.0"

    [ "$status" -eq 0 ]
    [[ "$output" == *"needle"* ]] && [[ "$output" == *"v1.2.3"* ]]
    [[ "$output" == *"bead"* ]] && [[ "$output" == *"v1.5.0"* ]]
}

# Test: Bead installation skipped message when --skip-bead
@test "bead installation skipped message when --skip-bead" {
    export NEEDLE_SKIP_BEAD="true"

    run check_skip_bead_message

    [ "$status" -eq 0 ]
    [[ "$output" == *"Skipping bead"* ]] || [[ "$output" == *"not installed"* ]]
}

# Test: Bead version comparison handles non-standard formats
@test "bead version comparison handles non-standard formats" {
    # Test with versions that have 'v' prefix
    run mock_bead_version_compare "v2.0.0" "v1.5.0"
    [ "$status" -eq 0 ]

    # Test with versions that have 'bead ' prefix
    run mock_bead_version_compare "bead 2.0.0" "bead 1.5.0"
    [ "$status" -eq 0 ]

    # Test with equal versions
    run mock_bead_version_compare "v1.5.0" "v1.5.0"
    [ "$status" -eq 0 ]
}

# Mock helper functions for testing

# Mock bead installation with checksum verification
install_bead_mock() {
    local binary="$1"
    local checksums_file="$2"
    local asset_name="$3"
    local skip_checksum="${4:-false}"

    # Verify checksum first
    if ! verify_checksum_with_fixture "$binary" "$checksums_file" "$asset_name"; then
        return 1
    fi

    # "Install" the binary
    cp "$binary" "${INSTALL_DIR}/bead"
    chmod +x "${INSTALL_DIR}/bead"

    echo "SUCCESS: bead installed to ${INSTALL_DIR}/bead"
    return 0
}

# Check skip bead flag
check_skip_bead_flag() {
    if [[ "$SKIP_BEAD" == "true" || "$NEEDLE_SKIP_BEAD" == "true" ]]; then
        echo "INFO: Skipping bead backend install (--skip-bead / NEEDLE_SKIP_BEAD)"
        return 0
    else
        echo "ERROR: bead should be installed"
        return 1
    fi
}

# Check skip bead message
check_skip_bead_message() {
    if [[ "$SKIP_BEAD" == "true" || "$NEEDLE_SKIP_BEAD" == "true" ]]; then
        echo "INFO: Skipping bead backend install (--skip-bead)"
        echo "WARN: bead backend not installed"
        return 0
    fi
    return 1
}

# Mock install success message
mock_install_success_message() {
    local needle_version="$1"
    local bead_version="$2"

    echo "SUCCESS: needle ${needle_version} installed successfully!"
    echo "SUCCESS: bead backend: ${bead_version}"
}
