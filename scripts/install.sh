#!/usr/bin/env bash
#
# NEEDLE Installer
# One-liner installation script for NEEDLE CLI
#
# Usage:
#   curl -fsSL https://needle.dev/install | bash
#   curl -fsSL https://needle.dev/install | bash -s -- --help
#
# Options:
#   --version VERSION     Install specific version (default: latest)
#   --install-dir DIR     Installation directory (default: ~/.local/bin)
#   --non-interactive     Skip all prompts
#   --no-modify-path      Don't modify shell rc files
#   --dry-run             Show what would be done without making changes
#   --uninstall           Remove NEEDLE installation
#   --help                Show this help message
#
# Environment variables:
#   NEEDLE_VERSION        Version to install (default: latest)
#   NEEDLE_INSTALL_DIR    Installation directory (default: ~/.local/bin)
#   NEEDLE_REPO           GitHub repository (default: needle-dev/needle)
#   NEEDLE_AUTO_INIT      Run 'needle init' after installation (true/false)
#   NEEDLE_NO_MODIFY_PATH Don't modify PATH (true/false)

set -euo pipefail

# -----------------------------------------------------------------------------
# Configuration
# -----------------------------------------------------------------------------

# Default values (can be overridden by environment or CLI args)
NEEDLE_VERSION="${NEEDLE_VERSION:-latest}"
NEEDLE_INSTALL_DIR="${NEEDLE_INSTALL_DIR:-$HOME/.local/bin}"
NEEDLE_REPO="${NEEDLE_REPO:-needle-dev/needle}"
NEEDLE_AUTO_INIT="${NEEDLE_AUTO_INIT:-false}"
NEEDLE_NO_MODIFY_PATH="${NEEDLE_NO_MODIFY_PATH:-false}"

# CLI flag overrides
NON_INTERACTIVE=false
DRY_RUN=false
UNINSTALL_MODE=false

# -----------------------------------------------------------------------------
# ANSI Colors
# -----------------------------------------------------------------------------

# Color support detection
if [[ -n "${NO_COLOR:-}" ]] || [[ ! -t 1 ]]; then
    RED='' GREEN='' BLUE='' YELLOW='' MAGENTA='' CYAN='' BOLD='' DIM='' NC=''
else
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    BLUE='\033[0;34m'
    YELLOW='\033[0;33m'
    MAGENTA='\033[0;35m'
    CYAN='\033[0;36m'
    BOLD='\033[1m'
    DIM='\033[2m'
    NC='\033[0m'
fi

# -----------------------------------------------------------------------------
# Output Functions
# -----------------------------------------------------------------------------

info() {
    printf '%bℹ%b %s\n' "$BLUE" "$NC" "$*"
}

success() {
    printf '%b✓%b %s\n' "$GREEN" "$NC" "$*"
}

warn() {
    printf '%b⚠%b %s\n' "$YELLOW" "$NC" >&2
}

error() {
    printf '%b✗%b %s\n' "$RED" "$NC" >&2
}

debug() {
    if [[ "${NEEDLE_DEBUG:-false}" == "true" ]]; then
        printf '%b[DEBUG]%b %s\n' "$DIM" "$NC" "$*"
    fi
}

header() {
    printf '\n'
    printf '%b▌ NEEDLE Installer%b\n' "$BOLD$MAGENTA" "$NC"
    printf '%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n' "$DIM" "$NC"
}

# -----------------------------------------------------------------------------
# Utility Functions
# -----------------------------------------------------------------------------

# Check if a command exists
command_exists() {
    command -v "$1" &>/dev/null
}

# Get current shell's rc file
get_shell_rc() {
    case "${SHELL:-}" in
        */bash)
            if [[ -f "$HOME/.bashrc" ]]; then
                echo "$HOME/.bashrc"
            elif [[ -f "$HOME/.bash_profile" ]]; then
                echo "$HOME/.bash_profile"
            fi
            ;;
        */zsh)
            echo "$HOME/.zshrc"
            ;;
        */fish)
            echo "$HOME/.config/fish/config.fish"
            ;;
        *)
            # Fallback to bashrc
            echo "$HOME/.bashrc"
            ;;
    esac
}

# Detect operating system
detect_os() {
    local os
    os=$(uname -s | tr '[:upper:]' '[:lower:]')

    case "$os" in
        linux*)
            echo "linux"
            ;;
        darwin*)
            echo "macos"
            ;;
        mingw*|msys*|cygwin*)
            error "Windows is not yet supported. Please use WSL."
            exit 1
            ;;
        *)
            error "Unsupported operating system: $os"
            exit 1
            ;;
    esac
}

# Detect CPU architecture
detect_arch() {
    local arch
    arch=$(uname -m)

    case "$arch" in
        x86_64|amd64)
            echo "amd64"
            ;;
        aarch64|arm64|armv8*)
            echo "arm64"
            ;;
        armv7*|armhf)
            echo "arm"
            ;;
        i386|i686)
            echo "386"
            ;;
        *)
            error "Unsupported architecture: $arch"
            exit 1
            ;;
    esac
}

# Check if directory is in PATH
in_path() {
    local dir="$1"
    [[ ":$PATH:" == *":$dir:"* ]]
}

# Get the latest release version from GitHub
get_latest_version() {
    local repo="$1"
    local url="https://api.github.com/repos/$repo/releases/latest"

    debug "Fetching latest version from $url"

    if command_exists curl; then
        curl -fsSL "$url" 2>/dev/null | grep -m1 '"tag_name"' | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/' | sed 's/^v//'
    elif command_exists wget; then
        wget -qO- "$url" 2>/dev/null | grep -m1 '"tag_name"' | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/' | sed 's/^v//'
    else
        error "Neither curl nor wget is available"
        exit 1
    fi
}

# Build download URL
build_download_url() {
    local repo="$1"
    local version="$2"
    local os="$3"
    local arch="$4"

    local version_path
    if [[ "$version" == "latest" ]]; then
        version_path="releases/latest/download"
    else
        version_path="releases/download/v${version#v}"
    fi

    echo "https://github.com/$repo/$version_path/needle-${os}-${arch}"
}

# Verify installation
verify_installation() {
    local binary="$1"

    if [[ ! -x "$binary" ]]; then
        return 1
    fi

    # Try to run version command
    if "$binary" version &>/dev/null; then
        return 0
    fi

    # If version command fails, check if file exists and is executable
    [[ -f "$binary" && -x "$binary" ]]
}

# -----------------------------------------------------------------------------
# Installation Functions
# -----------------------------------------------------------------------------

# Download and install NEEDLE
install_needle() {
    local os arch version url

    os=$(detect_os)
    arch=$(detect_arch)
    version="$NEEDLE_VERSION"

    info "Platform: $os/$arch"
    info "Version: $version"
    info "Install directory: $NEEDLE_INSTALL_DIR"

    # Build URL (skip version resolution in dry-run)
    if $DRY_RUN; then
        url=$(build_download_url "$NEEDLE_REPO" "$version" "$os" "$arch")
        info "[DRY RUN] Would download from: $url"
        info "[DRY RUN] Would install to: $NEEDLE_INSTALL_DIR/needle"
        return 0
    fi

    # Resolve 'latest' to actual version (only in actual install mode)
    if [[ "$version" == "latest" ]]; then
        info "Finding latest version..."
        version=$(get_latest_version "$NEEDLE_REPO")
        if [[ -z "$version" ]]; then
            warn "Could not determine latest version, using 'latest'"
            version="latest"
        else
            success "Latest version: $version"
        fi
    fi

    url=$(build_download_url "$NEEDLE_REPO" "$version" "$os" "$arch")

    # Create installation directory
    if [[ ! -d "$NEEDLE_INSTALL_DIR" ]]; then
        info "Creating installation directory..."
        mkdir -p "$NEEDLE_INSTALL_DIR"
        success "Created $NEEDLE_INSTALL_DIR"
    fi

    # Download binary
    info "Downloading NEEDLE..."
    debug "Download URL: $url"

    local binary="$NEEDLE_INSTALL_DIR/needle"
    local http_code

    if command_exists curl; then
        http_code=$(curl -fsSL -w "%{http_code}" -o "$binary" "$url" 2>/dev/null) || {
            error "Download failed"
            rm -f "$binary"
            exit 1
        }
    elif command_exists wget; then
        http_code=$(wget -q -O "$binary" "$url" 2>/dev/null && echo "200" || echo "000")
    else
        error "Neither curl nor wget is available"
        exit 1
    fi

    if [[ "$http_code" != "200" ]]; then
        error "Download failed with HTTP status: $http_code"
        rm -f "$binary"
        info "This may mean the release for your platform ($os/$arch) is not available yet."
        info "Check available releases at: https://github.com/$NEEDLE_REPO/releases"
        exit 1
    fi

    # Make executable
    chmod +x "$binary"
    success "Downloaded NEEDLE to $binary"

    # Verify
    if verify_installation "$binary"; then
        local installed_version
        installed_version=$("$binary" version 2>/dev/null | head -1 || echo "unknown")
        success "Installation verified: $installed_version"
    else
        warn "Could not verify installation, but binary was downloaded"
    fi

    # Handle PATH
    if ! in_path "$NEEDLE_INSTALL_DIR"; then
        if [[ "$NEEDLE_NO_MODIFY_PATH" != "true" ]]; then
            add_to_path "$NEEDLE_INSTALL_DIR"
        else
            warn "$NEEDLE_INSTALL_DIR is not in PATH"
            info "Add it manually: export PATH=\"$NEEDLE_INSTALL_DIR:\$PATH\""
        fi
    else
        success "$NEEDLE_INSTALL_DIR is already in PATH"
    fi

    return 0
}

# Add directory to PATH in shell rc
add_to_path() {
    local dir="$1"
    local shell_rc
    shell_rc=$(get_shell_rc)

    if [[ -z "$shell_rc" ]]; then
        warn "Could not determine shell rc file"
        return 1
    fi

    # Check if already added
    if [[ -f "$shell_rc" ]] && grep -q "needle.*PATH" "$shell_rc" 2>/dev/null; then
        debug "PATH entry already exists in $shell_rc"
        return 0
    fi

    info "Adding $dir to PATH in $shell_rc..."

    if $DRY_RUN; then
        info "[DRY RUN] Would add PATH entry to $shell_rc"
        return 0
    fi

    # Create rc file if it doesn't exist
    touch "$shell_rc"

    # Add PATH entry with comment
    {
        echo ""
        echo "# Added by NEEDLE installer"
        echo "export PATH=\"$dir:\$PATH\""
    } >> "$shell_rc"

    success "Updated $shell_rc"
    info "Run 'source $shell_rc' or restart your shell to apply changes"
}

# Uninstall NEEDLE
uninstall_needle() {
    local binary="$NEEDLE_INSTALL_DIR/needle"

    if [[ ! -f "$binary" ]]; then
        warn "NEEDLE is not installed at $binary"
        return 0
    fi

    info "Uninstalling NEEDLE..."

    if $DRY_RUN; then
        info "[DRY RUN] Would remove $binary"
        return 0
    fi

    rm -f "$binary"
    success "Removed $binary"

    # Optionally remove from PATH
    local shell_rc
    shell_rc=$(get_shell_rc)

    if [[ -f "$shell_rc" ]] && grep -q "needle.*PATH" "$shell_rc" 2>/dev/null; then
        if [[ "$NON_INTERACTIVE" == "true" ]] || [[ "$NEEDLE_NO_MODIFY_PATH" != "true" ]]; then
            info "Removing PATH entry from $shell_rc..."
            # Create a temp file without the NEEDLE PATH entry
            local temp_rc
            temp_rc=$(mktemp)
            grep -v "needle.*PATH" "$shell_rc" > "$temp_rc" || true
            grep -v "# Added by NEEDLE" "$temp_rc" > "$shell_rc"
            rm -f "$temp_rc"
            success "Removed PATH entry from $shell_rc"
        fi
    fi

    success "NEEDLE has been uninstalled"
}

# Run initialization
run_init() {
    local binary="$NEEDLE_INSTALL_DIR/needle"

    if [[ ! -x "$binary" ]]; then
        error "NEEDLE binary not found at $binary"
        return 1
    fi

    info "Running 'needle init'..."
    export PATH="$NEEDLE_INSTALL_DIR:$PATH"
    "$binary" init
}

# -----------------------------------------------------------------------------
# CLI Argument Parsing
# -----------------------------------------------------------------------------

show_help() {
    cat << 'EOF'
NEEDLE Installer - One-liner installation for NEEDLE CLI

Usage:
  curl -fsSL https://needle.dev/install | bash
  curl -fsSL https://needle.dev/install | bash -s -- [OPTIONS]

Options:
  --version VERSION     Install specific version (default: latest)
  --install-dir DIR     Installation directory (default: ~/.local/bin)
  --non-interactive     Skip all prompts and use defaults
  --no-modify-path      Don't modify shell rc files
  --dry-run             Show what would be done without making changes
  --uninstall           Remove NEEDLE installation
  --help, -h            Show this help message

Environment Variables:
  NEEDLE_VERSION        Version to install (default: latest)
  NEEDLE_INSTALL_DIR    Installation directory (default: ~/.local/bin)
  NEEDLE_REPO           GitHub repository (default: needle-dev/needle)
  NEEDLE_AUTO_INIT      Run 'needle init' after installation (true/false)
  NEEDLE_NO_MODIFY_PATH Don't modify PATH (true/false)

Examples:
  # Install latest version
  curl -fsSL https://needle.dev/install | bash

  # Install specific version
  curl -fsSL https://needle.dev/install | bash -s -- --version 0.1.0

  # Install to custom directory
  curl -fsSL https://needle.dev/install | bash -s -- --install-dir ~/bin

  # Non-interactive installation (for CI/CD)
  curl -fsSL https://needle.dev/install | bash -s -- --non-interactive

  # Uninstall
  curl -fsSL https://needle.dev/install | bash -s -- --uninstall

For more information, visit: https://github.com/needle-dev/needle
EOF
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --version)
                NEEDLE_VERSION="$2"
                shift 2
                ;;
            --install-dir)
                NEEDLE_INSTALL_DIR="$2"
                shift 2
                ;;
            --non-interactive|-y)
                NON_INTERACTIVE=true
                shift
                ;;
            --no-modify-path)
                NEEDLE_NO_MODIFY_PATH=true
                shift
                ;;
            --dry-run)
                DRY_RUN=true
                shift
                ;;
            --uninstall)
                UNINSTALL_MODE=true
                shift
                ;;
            --help|-h)
                show_help
                exit 0
                ;;
            --)
                shift
                break
                ;;
            -*)
                error "Unknown option: $1"
                show_help
                exit 1
                ;;
            *)
                break
                ;;
        esac
    done
}

# -----------------------------------------------------------------------------
# Main
# -----------------------------------------------------------------------------

main() {
    # Parse command line arguments
    parse_args "$@"

    # Show header
    header

    # Handle uninstall mode
    if $UNINSTALL_MODE; then
        uninstall_needle
        exit $?
    fi

    # Pre-flight checks
    info "Checking prerequisites..."

    if ! command_exists curl && ! command_exists wget; then
        error "Either curl or wget is required"
        exit 1
    fi
    success "Download tool available"

    if ! command_exists uname; then
        error "uname command not found"
        exit 1
    fi
    success "System detection available"

    # Install
    echo ""
    install_needle

    # Show completion message
    echo ""
    success "NEEDLE installed successfully!"
    echo ""

    if in_path "$NEEDLE_INSTALL_DIR"; then
        printf '%bUsage:%b needle <command>\n' "$BOLD" "$NC"
        printf '%bExamples:%b\n' "$BOLD" "$NC"
        printf '  needle init        Initialize NEEDLE configuration\n'
        printf '  needle version     Show version information\n'
        printf '  needle help        Show all available commands\n'
    else
        printf '%bTo use NEEDLE, add it to your PATH:%b\n' "$BOLD" "$NC"
        printf '  export PATH="%s:$PATH"\n' "$NEEDLE_INSTALL_DIR"
        printf '\nOr restart your shell to apply changes.\n'
    fi

    # Optional auto-init
    if [[ "$NEEDLE_AUTO_INIT" == "true" ]]; then
        echo ""
        run_init
    else
        echo ""
        info "Run 'needle init' to complete setup"
    fi
}

# Run main
main "$@"
