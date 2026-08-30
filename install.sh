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
BEAD_REPO="jedarden/bead-rs"
BEAD_API="https://api.github.com/repos/$BEAD_REPO/releases/latest" # gitleaks:allow - public API endpoint
SKIP_BEAD="${NEEDLE_SKIP_BEAD:-false}"
BEAD_INSTALLED_VERSION=""

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

SECURITY NOTE — CHECKSUM VERIFICATION:
  By default, the installer verifies the downloaded binary's SHA-256 checksum
  against the release metadata. Installation aborts if verification fails.

  Opt-out (NOT RECOMMENDED): --skip-checksum or NEEDLE_SKIP_CHECKSUM=1
    WARNING: Skipping verification is a SECURITY RISK. Only use this in
    trusted environments (air-gapped networks, local development) where you
    have alternative verification methods and accept the risk of installing a
    tampered binary.

  Important: Actual checksum MISMATCHES are never skippable, even with
  --skip-checksum. This flag only applies when checksums are unavailable,
  not when they indicate a mismatch.

BEAD BACKEND:
  needle cannot open a workspace without the bead-rs CLI (`bead`). The
  installer downloads it from the jedarden/bead-rs release for this platform,
  verifies its checksum the same way, and installs it next to needle. An
  existing `bead` on PATH that is at least the release version is kept.

  Opt-out: --skip-bead or NEEDLE_SKIP_BEAD=1

OPTIONS:
    -h, --help              Show this help message
    --skip-checksum         Skip checksum verification (SECURITY RISK)
    --skip-bead             Do not install the bead-rs backend (bead) alongside needle

ENVIRONMENT VARIABLES:
    NEEDLE_INSTALL_PATH     Installation path (default: ~/.local/bin/needle)
    NEEDLE_SKIP_CHECKSUM    Set to '1' or 'true' to skip checksum (SECURITY RISK)
    NEEDLE_SKIP_BEAD        Set to '1' or 'true' to skip installing the bead backend

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
            --skip-bead)
                export NEEDLE_SKIP_BEAD="true"
                SKIP_BEAD="true"
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
    if [[ "$SKIP_BEAD" == "1" || "$SKIP_BEAD" == "true" || "$SKIP_BEAD" == "yes" ]]; then
        SKIP_BEAD="true"
    else
        SKIP_BEAD="false"
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

# Fetch a GitHub "latest release" document. The whole document goes into a
# variable: curl must never be piped into an early-exiting reader (grep -m1,
# head) — the reader closes the pipe while curl still has data to write, which
# produced `curl: (23) Failure writing output to destination` and, under
# `set -o pipefail`, can turn the writer's SIGPIPE into an abort.
fetch_release_json() {
    local url="$1"
    if command -v curl &>/dev/null; then
        curl -fsSL "$url" 2>/dev/null
    elif command -v wget &>/dev/null; then
        wget -qO- "$url" 2>/dev/null
    else
        return 2
    fi
}

# Print the tag_name of a release document, or nothing.
extract_tag() {
    local tag_re='"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)"'
    if [[ "$1" =~ $tag_re ]]; then
        printf '%s\n' "${BASH_REMATCH[1]}"
    fi
}

# Print every "name" value in a release document, one per line. grep reads the
# whole here-string, so there is no early-exiting reader here either.
list_release_assets() {
    grep -oE '"name"[[:space:]]*:[[:space:]]*"[^"]+"' <<<"$1" 2>/dev/null \
        | sed -E 's/^"name"[[:space:]]*:[[:space:]]*"([^"]+)"$/\1/' || true
}

# asset_listed <release-json> <asset-name>
# 0 when the asset is listed. A document with no names at all is treated as
# "cannot judge" and also returns 0 — the download itself reports a 404.
asset_listed() {
    local names
    names=$(list_release_assets "$1")
    [[ -z "$names" ]] && return 0
    grep -qxF "$2" <<<"$names"
}

# Get the latest release version from GitHub and check asset availability
get_latest_version() {
    local version
    local api_output

    if ! command -v curl &>/dev/null && ! command -v wget &>/dev/null; then
        error "Neither curl nor wget is available. Please install one of them."
    fi
    api_output=$(fetch_release_json "$GITHUB_API") ||
        error "Could not reach the GitHub API to determine the latest version. Please check your internet connection."

    version=$(extract_tag "$api_output")
    if [[ -z "$version" ]]; then
        error "Failed to determine the latest version. Please check your internet connection."
    fi

    # Check if the required asset exists in the release
    check_asset_available "$api_output" "$1" "$version" || return 1

    echo "$version"
}

# Check if the required asset exists in the release JSON
check_asset_available() {
    local api_output="$1"
    local asset_name="$2"
    local version="$3"

    if ! asset_listed "$api_output" "$asset_name"; then
        cat >&2 <<EOF
No prebuilt binary for ${asset_name} in ${version}.
Prebuilt targets: x86_64-unknown-linux-gnu.
Build from source: cargo install --git https://github.com/jedarden/NEEDLE
EOF
        return 1
    fi

    return 0
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

# download_and_verify_checksums <repo> <version> <file> <asset> <temp_dir>
# Download <repo>'s checksums.txt for <version> into <temp_dir> and verify
# <file> against its <asset> entry. Fail-closed, with the --skip-checksum
# semantics documented in --help. Shared by the needle and bead installs.
download_and_verify_checksums() {
    local repo="$1" version="$2" file="$3" asset="$4" temp_dir="$5"
    # Download and verify checksums (fail-closed: verification enabled by default for security)
    local checksums_url="https://github.com/${repo}/releases/download/${version}/checksums.txt"
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
        expected_hash=$(grep "  ${asset}$\| ${asset}$" "$checksums_file" | awk '{print $1}' || true)
        if [[ -z "$expected_hash" ]]; then
            if [[ "$SKIP_CHECKSUM" == "true" ]]; then
                warn_checksum_skipped
                warn "Skipping checksum verification (checksum for ${asset} not found)"
            else
                error "Could not find checksum for ${asset} in checksums.txt. Installation aborted for security reasons."
            fi
        else
            # We have an expected hash, compute the actual hash
            local actual_hash=""
            local found_hash_tool=false
            if command -v sha256sum &>/dev/null; then
                actual_hash=$(sha256sum "$file" | awk '{print $1}')
                found_hash_tool=true
            elif command -v shasum &>/dev/null; then
                actual_hash=$(shasum -a 256 "$file" | awk '{print $1}')
                found_hash_tool=true
            elif command -v openssl &>/dev/null; then
                actual_hash=$(openssl dgst -sha256 "$file" | awk '{print $2}')
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
                    error "Checksum mismatch for ${asset}!
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
}

# version_ge <a> <b>: 0 when dotted version a >= b (leading "v" ignored).
version_ge() {
    local a="${1#v}" b="${2#v}"
    local IFS=.
    local -a pa=($a) pb=($b)
    local i x y
    for i in 0 1 2; do
        x="${pa[$i]:-0}"; y="${pb[$i]:-0}"
        x="${x%%[^0-9]*}"; y="${y%%[^0-9]*}"
        if (( ${x:-0} > ${y:-0} )); then return 0; fi
        if (( ${x:-0} < ${y:-0} )); then return 1; fi
    done
    return 0
}

# install_bead <arch> <os> <install_dir> <temp_dir>
# Install the bead-rs backend (`bead`) next to needle. needle cannot open a
# workspace without it (GitHub #16). Unreachable release or no build for this
# platform: warn and continue. Checksum problems: same fail-closed rules as the
# needle binary.
install_bead() {
    local arch="$1" os="$2" install_dir="$3" temp_dir="$4"

    if [[ "$SKIP_BEAD" == "true" ]]; then
        info "Skipping bead backend install (--skip-bead / NEEDLE_SKIP_BEAD)"
        return 0
    fi

    info "Installing bead backend (bead-rs)..."
    local api_output bead_version
    if ! api_output=$(fetch_release_json "$BEAD_API"); then
        warn "Could not reach the GitHub API for ${BEAD_REPO}; bead not installed."
        warn "Install it later: cargo install --git https://github.com/${BEAD_REPO} --bin bead"
        return 0
    fi
    bead_version=$(extract_tag "$api_output")
    if [[ -z "$bead_version" ]]; then
        warn "Could not determine the latest ${BEAD_REPO} release; bead not installed."
        return 0
    fi

    # Never downgrade a bead the user already has.
    if command -v bead &>/dev/null; then
        local existing
        existing=$(bead --version 2>/dev/null | awk '{print $2}')
        if [[ -n "$existing" ]] && version_ge "$existing" "$bead_version"; then
            info "bead ${existing} already on PATH ($(command -v bead)) — keeping it (release is ${bead_version})"
            BEAD_INSTALLED_VERSION="$existing"
            return 0
        fi
    fi

    local asset="bead-${arch}-${os}"
    if ! asset_listed "$api_output" "$asset"; then
        warn "No prebuilt bead for ${arch}-${os} in ${bead_version}; bead not installed."
        warn "Build it from source: cargo install --git https://github.com/${BEAD_REPO} --bin bead"
        return 0
    fi

    local bead_tmp="$temp_dir/bead-rs"
    mkdir -p "$bead_tmp"
    local temp_bead="$bead_tmp/bead"
    if ! download_file "https://github.com/${BEAD_REPO}/releases/download/${bead_version}/${asset}" "$temp_bead" 2>/dev/null; then
        warn "Could not download ${asset} from ${BEAD_REPO} ${bead_version}; bead not installed."
        return 0
    fi

    download_and_verify_checksums "$BEAD_REPO" "$bead_version" "$temp_bead" "$asset" "$bead_tmp"

    chmod +x "$temp_bead"
    if ! "$temp_bead" --version &>/dev/null; then
        error "Downloaded bead binary is not executable or corrupted."
    fi
    mv "$temp_bead" "${install_dir}/bead"
    BEAD_INSTALLED_VERSION="$bead_version"
    success "bead ${bead_version} installed to ${install_dir}/bead"
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

    # Get latest version and check asset availability
    version=$(get_latest_version "$asset_name") || exit 1
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
    local checksums_file="$temp_dir/checksums.txt"
    download_and_verify_checksums "$REPO" "$version" "$temp_binary" "$asset_name" "$temp_dir"

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

    # The bead backend goes next to needle (GitHub #16).
    install_bead "$arch" "$os" "$install_dir" "$temp_dir"

    # Check if install dir is in PATH
    local path_has_dir=false
    if [[ ":$PATH:" == *":$install_dir:"* ]]; then
        path_has_dir=true
    fi

    # Success message
    success "needle $version installed successfully!"
    if [[ -n "$BEAD_INSTALLED_VERSION" ]]; then
        success "bead backend: ${BEAD_INSTALLED_VERSION}"
    elif [[ "$SKIP_BEAD" != "true" ]]; then
        warn "bead backend not installed — 'needle doctor' will fail until 'bead' is on PATH."
    fi

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
