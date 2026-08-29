#!/bin/bash
#
# NEEDLE Installer
# https://github.com/jedarden/NEEDLE
#
# Usage:
#   curl -fsSL https://github.com/jedarden/NEEDLE/releases/latest/download/install.sh | bash
#
# Downloads the latest needle binary for the detected platform and installs
# it to ~/.local/bin/needle (or $NEEDLE_INSTALL_PATH if set).

set -euo pipefail

# Configuration
REPO="jedarden/NEEDLE"
INSTALL_PATH="${NEEDLE_INSTALL_PATH:-$HOME/.local/bin/needle}"
GITHUB_API="https://api.github.com/repos/$REPO/releases/latest" # gitleaks:allow - public API endpoint
SKIP_CHECKSUM="${NEEDLE_SKIP_CHECKSUM:-false}"

# Colors (only if stdout is a terminal)
if [[ -t 1 ]]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    BLUE='\033[0;34m'
    NC='\033[0m' # No Color
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    NC=''
fi

info() {
    echo -e "${BLUE}==>${NC} $1"
}

success() {
    echo -e "${GREEN}==>${NC} $1"
}

warn() {
    echo -e "${YELLOW}==>${NC} $1" >&2
}

error() {
    echo -e "${RED}Error:${NC} $1" >&2
    exit 1
}

# Conspicuous warning for checksum opt-out
warn_checksum_skipped() {
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
    # Prompt only when stdin is a terminal. The documented pipe invocation
    # (curl ... | bash -s -- --skip-checksum) has no stdin left to read:
    # `read -r` returns 1 at EOF, which under `set -e` aborted the install
    # right after this warning instead of proceeding with the opt-out.
    if [[ -t 0 ]]; then
        read -r || true
    fi
}

# Show usage information
show_usage() {
    cat <<EOF
Usage: install.sh [OPTIONS]

NEEDLE installer downloads and verifies the latest release binary.

CHECKSUM VERIFICATION:
  By default, the installer verifies the downloaded binary's SHA-256 checksum
  against the release metadata. Installation aborts if verification fails.

  Opt-out: --skip-checksum or NEEDLE_SKIP_CHECKSUM=1
    WARNING: Skipping verification is a SECURITY RISK. Only use this in
    trusted environments (air-gapped networks, local development) where you
    have alternative verification methods and accept the risk of installing a
    tampered binary.

  Important: Actual checksum MISMATCHES are never skippable, even with
  --skip-checksum. This flag only applies when checksums are unavailable,
  not when they indicate a mismatch.

OPTIONS:
    -h, --help              Show this help message
    --skip-checksum         Skip checksum verification (SECURITY RISK)

ENVIRONMENT VARIABLES:
    NEEDLE_INSTALL_PATH     Installation path (default: ~/.local/bin/needle)
    NEEDLE_SKIP_CHECKSUM    Set to '1' or 'true' to skip checksum (SECURITY RISK)

EXAMPLES:
    # Normal installation (recommended)
    curl -fsSL https://github.com/jedarden/NEEDLE/releases/latest/download/install.sh | bash

    # Skip checksum verification (NOT recommended, use with caution)
    curl -fsSL https://github.com/jedarden/NEEDLE/releases/latest/download/install.sh | bash -s -- --skip-checksum

    # Skip via environment variable (NOT recommended, use with caution)
    NEEDLE_SKIP_CHECKSUM=1 curl -fsSL https://github.com/jedarden/NEEDLE/releases/latest/download/install.sh | bash
EOF
}

# Parse command-line arguments
parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -h|--help)
                show_usage
                exit 0
                ;;
            --skip-checksum)
                # Normalize the env var if set via flag
                export NEEDLE_SKIP_CHECKSUM="true"
                SKIP_CHECKSUM="true"
                ;;
            *)
                error "Unknown option: $1. Use --help for usage."
                ;;
        esac
        shift
    done

    # Normalize environment variable values
    if [[ "$SKIP_CHECKSUM" == "1" || "$SKIP_CHECKSUM" == "true" || "$SKIP_CHECKSUM" == "yes" ]]; then
        SKIP_CHECKSUM="true"
    else
        SKIP_CHECKSUM="false"
    fi
}

# Detect the operating system
detect_os() {
    case "$(uname -s)" in
        Linux*)  echo "unknown-linux-gnu" ;;
        Darwin*) echo "apple-darwin" ;;
        *)       error "Unsupported OS: $(uname -s)" ;;
    esac
}

# Detect the CPU architecture
detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64) echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *)             error "Unsupported architecture: $(uname -m)" ;;
    esac
}

# Get the latest release version from GitHub
get_latest_version() {
    local version
    local api_output

    # Fetch the whole release document into a variable first. curl must not
    # be piped into an early-exiting reader (grep -m1, head): the reader
    # closes the pipe while curl still has data to write, which produced
    # `curl: (23) Failure writing output to destination` and, under
    # `set -o pipefail`, can turn the writer's SIGPIPE into an abort.
    if command -v curl &>/dev/null; then
        api_output=$(curl -fsSL "$GITHUB_API" 2>/dev/null) ||
            error "Could not reach the GitHub API to determine the latest version. Please check your internet connection."
    elif command -v wget &>/dev/null; then
        api_output=$(wget -qO- "$GITHUB_API" 2>/dev/null) ||
            error "Could not reach the GitHub API to determine the latest version. Please check your internet connection."
    else
        error "Neither curl nor wget is available. Please install one of them."
    fi

    # Extract the tag with a regex match instead of a grep pipeline, so no
    # reader can exit early and close a pipe under the writer's feet.
    local tag_re='"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)"'
    if [[ "$api_output" =~ $tag_re ]]; then
        version="${BASH_REMATCH[1]}"
    else
        version=""
    fi

    if [[ -z "$version" ]]; then
        error "Failed to determine the latest version. Please check your internet connection."
    fi

    echo "$version"
}

# Download a file using curl or wget
download_file() {
    local url="$1"
    local output="$2"

    info "Downloading $url..."

    if command -v curl &>/dev/null; then
        curl -fsSL --progress-bar -o "$output" "$url"
    elif command -v wget &>/dev/null; then
        wget -q --show-progress -O "$output" "$url"
    else
        error "Neither curl nor wget is available."
    fi
}

# Main installation logic
main() {
    parse_args "$@"
    info "Installing needle..."

    # Detect platform
    local os arch asset_name download_url version
    os=$(detect_os)
    arch=$(detect_arch)
    asset_name="needle-${arch}-${os}"

    info "Detected platform: ${arch}-${os}"

    # Get latest version
    version=$(get_latest_version)
    info "Latest version: $version"

    # Construct download URL
    download_url="https://github.com/${REPO}/releases/download/${version}/${asset_name}"

    # Create temporary directory for download.
    #
    # The trap must bake in the PATH, not defer expansion: `temp_dir` is a
    # `main`-local, so by the time an EXIT trap runs the shell has left that
    # scope and `$temp_dir` is unset. Under `set -u` that made the trap itself
    # fail with "temp_dir: unbound variable", so the installer exited 1 after a
    # completely successful install and skipped its own cleanup.
    local temp_dir
    temp_dir=$(mktemp -d)
    trap "rm -rf '$temp_dir'" EXIT

    local temp_binary="$temp_dir/needle"

    # Download the binary
    download_file "$download_url" "$temp_binary"

    # Download and verify checksums (fail-closed: verification enabled by default for security)
    local checksums_url="https://github.com/${REPO}/releases/download/${version}/checksums.txt"
    local checksums_file="$temp_dir/checksums.txt"
    info "Downloading checksums..."
    if ! download_file "$checksums_url" "$checksums_file" 2>/dev/null; then
        if [[ "$SKIP_CHECKSUM" == "true" ]]; then
            warn_checksum_skipped
            warn "Skipping checksum verification (checksums.txt unavailable)"
        else
            error "Could not download checksums.txt. Installation aborted for security reasons."
        fi
    else
        # Checksums downloaded successfully, proceed with verification
        info "Verifying checksum..."
        local expected_hash
        # `|| true`: grep exits 1 when the asset has no entry, and under
        # `set -euo pipefail` that would abort the installer silently here —
        # before the legible error below and before the --skip-checksum
        # branch could apply. An empty result is handled explicitly.
        expected_hash=$(grep "  ${asset_name}$\| ${asset_name}$" "$checksums_file" | awk '{print $1}' || true)
        if [[ -z "$expected_hash" ]]; then
            if [[ "$SKIP_CHECKSUM" == "true" ]]; then
                warn_checksum_skipped
                warn "Skipping checksum verification (checksum for ${asset_name} not found)"
            else
                error "Could not find checksum for ${asset_name} in checksums.txt. Installation aborted for security reasons."
            fi
        else
            # We have an expected hash, compute the actual hash
            local actual_hash=""
            local found_hash_tool=false
            if command -v sha256sum &>/dev/null; then
                actual_hash=$(sha256sum "$temp_binary" | awk '{print $1}')
                found_hash_tool=true
            elif command -v shasum &>/dev/null; then
                actual_hash=$(shasum -a 256 "$temp_binary" | awk '{print $1}')
                found_hash_tool=true
            elif command -v openssl &>/dev/null; then
                actual_hash=$(openssl dgst -sha256 "$temp_binary" | awk '{print $2}')
                found_hash_tool=true
            fi

            if [[ "$found_hash_tool" == "false" ]]; then
                if [[ "$SKIP_CHECKSUM" == "true" ]]; then
                    warn_checksum_skipped
                    warn "Skipping checksum verification (no hash tool available)"
                else
                    error "No SHA-256 tool found. Need sha256sum, shasum, or openssl.
Installation aborted for security reasons."
                fi
            elif [[ -z "$actual_hash" ]]; then
                if [[ "$SKIP_CHECKSUM" == "true" ]]; then
                    warn_checksum_skipped
                    warn "Skipping checksum verification (failed to compute checksum)"
                else
                    error "Failed to compute checksum for downloaded binary. Installation aborted for security reasons."
                fi
            else
                # Verify checksum matches - MISMATCHES ARE NEVER SKIPPABLE (security-critical)
                if [[ "$actual_hash" != "$expected_hash" ]]; then
                    error "Checksum mismatch for ${asset_name}!
  expected: ${expected_hash}
  got:      ${actual_hash}

The downloaded binary may be corrupted or tampered with.
Installation aborted for security reasons.

NOTE: Checksum mismatches are never skippable, even with --skip-checksum.
This flag only applies when checksums are unavailable, not when they indicate a mismatch."
                fi
                success "Checksum verified."
            fi
        fi
    fi

    # Optional GPG signature verification (informational only, never fails)
    if command -v gpg &>/dev/null; then
        local sig_url="https://github.com/${REPO}/releases/download/${version}/checksums.txt.asc"
        local sig_file="$temp_dir/checksums.txt.asc"
        if download_file "$sig_url" "$sig_file" 2>/dev/null; then
            info "Verifying GPG signature..."
            if gpg --verify "$sig_file" "$checksums_file" 2>/dev/null; then
                success "GPG signature verified."
            else
                warn "GPG signature verification failed (signing key may not be in your keyring)."
            fi
        fi
    fi

    # Make it executable
    chmod +x "$temp_binary"

    # Verify the binary works
    info "Verifying binary..."
    if ! "$temp_binary" --version &>/dev/null; then
        error "Downloaded binary is not executable or corrupted."
    fi

    # Create installation directory if needed
    local install_dir
    install_dir=$(dirname "$INSTALL_PATH")
    mkdir -p "$install_dir"

    # Move binary into place
    info "Installing to $INSTALL_PATH..."
    mv "$temp_binary" "$INSTALL_PATH"

    # Download and install transform binaries alongside needle.
    local transforms=("needle-transform-claude" "needle-transform-codex")
    for transform in "${transforms[@]}"; do
        local transform_asset="${transform}-${arch}-${os}"
        local transform_url="https://github.com/${REPO}/releases/download/${version}/${transform_asset}"
        local transform_dest="${install_dir}/${transform}"
        local temp_transform="$temp_dir/${transform}"

        info "Installing ${transform}..."
        if download_file "$transform_url" "$temp_transform" 2>/dev/null; then
            chmod +x "$temp_transform"
            mv "$temp_transform" "$transform_dest"
            success "${transform} installed to ${transform_dest}"
        else
            warn "${transform} not found in release assets — skipping (needle doctor will warn if referenced by an adapter)"
        fi
    done

    # Check if install dir is in PATH
    local path_has_dir=false
    if [[ ":$PATH:" == *":$install_dir:"* ]]; then
        path_has_dir=true
    fi

    # Success message
    success "needle $version installed successfully!"

    if [[ "$path_has_dir" == true ]]; then
        echo ""
        echo "Run 'needle --help' to get started."
    else
        echo ""
        warn "$install_dir is not in your PATH."
        echo ""
        echo "Add it to your PATH by adding this line to your shell profile (~/.bashrc, ~/.zshrc, etc.):"
        echo ""
        echo "    export PATH=\"\$PATH:$install_dir\""
        echo ""
        echo "Then run 'source ~/.bashrc' (or your shell profile) and try 'needle --help'."
    fi
}

# Run main
main "$@"
