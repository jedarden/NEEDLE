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
GITHUB_API="https://api.github.com/repos/$REPO/releases/latest"

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

    if command -v curl &>/dev/null; then
        version=$(curl -fsSL "$GITHUB_API" | grep -m1 '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')
    elif command -v wget &>/dev/null; then
        version=$(wget -qO- "$GITHUB_API" | grep -m1 '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')
    else
        error "Neither curl nor wget is available. Please install one of them."
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

    # Create temporary directory for download
    local temp_dir
    temp_dir=$(mktemp -d)
    trap 'rm -rf "$temp_dir"' EXIT

    local temp_binary="$temp_dir/needle"

    # Download the binary
    download_file "$download_url" "$temp_binary"

    # Download and verify checksums
    local checksums_url="https://github.com/${REPO}/releases/download/${version}/checksums.txt"
    local checksums_file="$temp_dir/checksums.txt"
    info "Downloading checksums..."
    if download_file "$checksums_url" "$checksums_file" 2>/dev/null; then
        info "Verifying checksum..."
        local expected_hash
        expected_hash=$(grep "  ${asset_name}$\| ${asset_name}$" "$checksums_file" | awk '{print $1}')
        if [[ -z "$expected_hash" ]]; then
            warn "Could not find checksum for ${asset_name} in checksums.txt — skipping"
        else
            local actual_hash=""
            if command -v sha256sum &>/dev/null; then
                actual_hash=$(sha256sum "$temp_binary" | awk '{print $1}')
            elif command -v shasum &>/dev/null; then
                actual_hash=$(shasum -a 256 "$temp_binary" | awk '{print $1}')
            else
                warn "Neither sha256sum nor shasum found — skipping checksum verification"
            fi
            if [[ -n "$actual_hash" ]]; then
                if [[ "$actual_hash" != "$expected_hash" ]]; then
                    error "Checksum mismatch for ${asset_name}!
  expected: ${expected_hash}
  got:      ${actual_hash}"
                fi
                success "Checksum verified."
            fi
        fi

        # Optional GPG signature verification
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
    else
        warn "Could not download checksums.txt — skipping integrity check"
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
