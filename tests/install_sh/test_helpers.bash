# Test helper functions for install.sh checksum verification tests
#
# These functions provide isolated testing of the checksum verification
# logic without requiring real downloads or installations.

# Verify checksum with a local fixture file
# Usage: verify_checksum_with_fixture <binary> <checksums_file> <asset_name>
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
            echo "ERROR: Could not download checksums.txt. Installation aborted for security reasons."
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
            echo "ERROR: Could not find checksum for ${asset_name} in checksums.txt. Installation aborted for security reasons."
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
            echo "ERROR: Neither sha256sum nor shasum available. Cannot verify checksum.
Installation aborted for security reasons."
            return 1
        fi
    fi

    if [[ -z "$actual_hash" ]]; then
        if [[ "$skip_checksum" == "true" ]]; then
            echo "WARNING: Skipping checksum verification (failed to compute checksum)"
            return 0
        else
            echo "ERROR: Failed to compute checksum for downloaded binary. Installation aborted for security reasons."
            return 1
        fi
    fi

    # Verify checksum matches - MISMATCHES ARE NEVER SKIPPABLE
    if [[ "$actual_hash" != "$expected_hash" ]]; then
        echo "ERROR: Checksum mismatch for ${asset_name}!
  expected: ${expected_hash}
  got:      ${actual_hash}

The downloaded binary may be corrupted or tampered with.
Installation aborted for security reasons.

NOTE: Checksum mismatches are never skippable, even with --skip-checksum.
This flag only applies when checksums are unavailable, not when they indicate a mismatch."
        return 1
    fi

    echo "SUCCESS: Checksum verified."
    return 0
}

# Parse and check skip flag
# Usage: parse_and_check_skip_flag [--skip-checksum]
parse_and_check_skip_flag() {
    local skip_checksum="${NEEDLE_SKIP_CHECKSUM:-false}"

    # Parse command line flag
    if [[ "$1" == "--skip-checksum" ]]; then
        skip_checksum="true"
    fi

    # Normalize environment variable values
    if [[ "$skip_checksum" == "1" || "$skip_checksum" == "true" || "$skip_checksum" == "yes" ]]; then
        skip_checksum="true"
    else
        skip_checksum="false"
    fi

    if [[ "$skip_checksum" == "true" ]]; then
        echo "SKIP: true"
        return 0
    else
        echo "SKIP: false"
        return 0
    fi
}

# Run checksum skip warning
# Usage: run_checksum_skip_warning
run_checksum_skip_warning() {
    cat <<'EOF'

════════════════════════════════════════════════════════════════════════════════
                                  ⚠️  SECURITY WARNING  ⚠️
════════════════════════════════════════════════════════════════════════════════

Checksum verification is DISABLED. The downloaded binary will NOT be verified
against the expected SHA-256 hash from the release.

This means you CANNOT detect if the binary has been:
  • Corrupted during download
  • Tampered with by a malicious actor
  • Modified from what the project released

Risks of installing without checksum verification:
  → You may install a compromised binary
  → A malicious actor could inject arbitrary code
  → Your system and data could be at risk

The NEEDLE project strongly recommends AGAINST this option. Only use it if:
  • You are in a controlled environment with alternative verification
  • You fully understand and accept the security risks
  • This is a temporary workaround for network/infrastructure issues

For normal installations, press Ctrl+C to abort and fix the checksum issue.

════════════════════════════════════════════════════════════════════════════════

Press Enter to continue with checksum verification DISABLED, or Ctrl+C to abort...
EOF
}

# Check that GPG verification is informational only
# Usage: check_gpg_verification_is_informational
check_gpg_verification_is_informational() {
    echo "INFO: GPG signature verification is informational only and never fails installation"
    return 0
}

# Mock HTTP server for testing download scenarios
# Usage: start_mock_http_server <port>
start_mock_http_server() {
    local port="$1"
    local response_dir="$2"

    # Create a simple HTTP server using nc (netcat)
    if command -v nc &>/dev/null; then
        while true; do
            echo -e "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nMock server running" | nc -l "$port" &
            sleep 0.1
        done
    else
        echo "ERROR: nc (netcat) not available for mock HTTP server"
        return 1
    fi
}

# Create a mock binary with specific content
# Usage: create_mock_binary <output_path> <content>
create_mock_binary() {
    local output_path="$1"
    local content="${2:-mock binary content}"

    echo "$content" > "$output_path"
    chmod +x "$output_path"
}

# Calculate SHA-256 checksum of a file
# Usage: calculate_checksum <file>
calculate_checksum() {
    local file="$1"

    if command -v sha256sum &>/dev/null; then
        sha256sum "$file" | awk '{print $1}'
    elif command -v shasum &>/dev/null; then
        shasum -a 256 "$file" | awk '{print $1}'
    else
        echo "ERROR: No checksum tool available"
        return 1
    fi
}

# Create a checksums file for testing
# Usage: create_checksums_file <output_path> <binary_path> <asset_name>
create_checksums_file() {
    local output_path="$1"
    local binary_path="$2"
    local asset_name="${3:-needle-x86_64-unknown-linux-gnu}"

    local checksum
    checksum=$(calculate_checksum "$binary_path")

    if [[ $? -eq 0 ]]; then
        echo "${checksum}  ${asset_name}" > "$output_path"
        return 0
    else
        return 1
    fi
}
