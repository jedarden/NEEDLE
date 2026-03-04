#!/usr/bin/env bash
#
# NEEDLE Installer
# One-liner installation script for NEEDLE CLI
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/anthropics/needle/main/scripts/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/anthropics/needle/main/scripts/install.sh | bash -s -- --help
#
# Options:
#   --version VERSION     Install specific version (default: latest)
#   --install-dir DIR     Installation directory (default: ~/.needle)
#   --non-interactive     Skip all prompts
#   --no-modify-path      Don't modify shell rc files
#   --dry-run             Show what would be done without making changes
#   --uninstall           Remove NEEDLE installation
#   --help                Show this help message
#
# Environment variables:
#   NEEDLE_VERSION        Version to install (default: latest)
#   NEEDLE_INSTALL_DIR    Installation directory (default: ~/.needle)
#   NEEDLE_REPO           GitHub repository (default: anthropics/needle)
#   NEEDLE_NO_MODIFY_PATH Don't modify PATH (true/false)

set -euo pipefail

# -----------------------------------------------------------------------------
# Configuration
# -----------------------------------------------------------------------------

# Default values (can be overridden by environment or CLI args)
NEEDLE_VERSION="${NEEDLE_VERSION:-latest}"
NEEDLE_INSTALL_DIR="${NEEDLE_INSTALL_DIR:-$HOME/.needle}"
NEEDLE_REPO="${NEEDLE_REPO:-anthropics/needle}"
NEEDLE_NO_MODIFY_PATH="${NEEDLE_NO_MODIFY_PATH:-false}"
NEEDLE_BIN_DIR="${NEEDLE_BIN_DIR:-$HOME/.local/bin}"

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
    printf '%b⚠%b %s\n' "$YELLOW" "$NC" "$*" >&2
}

error() {
    printf '%b✗%b %s\n' "$RED" "$NC" "$*" >&2
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

# Build release download URL
build_download_url() {
    local repo="$1"
    local version="$2"

    if [[ "$version" == "latest" ]]; then
        echo "https://github.com/$repo/releases/latest/download/needle"
    else
        echo "https://github.com/$repo/releases/download/v${version#v}/needle"
    fi
}

# -----------------------------------------------------------------------------
# Installation Functions
# -----------------------------------------------------------------------------

# Download and install NEEDLE
install_needle() {
    local version url

    version="$NEEDLE_VERSION"

    info "Version: $version"
    info "Install directory: $NEEDLE_INSTALL_DIR"

    # Build URL (skip version resolution in dry-run)
    if $DRY_RUN; then
        url=$(build_download_url "$NEEDLE_REPO" "$version")
        info "[DRY RUN] Would download from: $url"
        info "[DRY RUN] Would install to: $NEEDLE_INSTALL_DIR"
        return 0
    fi

    # Resolve 'latest' to actual version
    if [[ "$version" == "latest" ]]; then
        info "Finding latest version..."
        version=$(get_latest_version "$NEEDLE_REPO") || true
        if [[ -z "$version" ]]; then
            warn "Could not determine latest version, using main branch"
            version="latest"
        else
            success "Latest version: $version"
        fi
    fi

    url=$(build_download_url "$NEEDLE_REPO" "$version")

    # Create temp directory for download
    local tmp_dir
    tmp_dir=$(mktemp -d)
    trap "rm -rf '$tmp_dir'" EXIT

    # Download single-file binary
    info "Downloading NEEDLE..."
    debug "Download URL: $url"

    local needle_file="$tmp_dir/needle"

    if command_exists curl; then
        curl -fsSL -o "$needle_file" "$url" || {
            error "Download failed"
            exit 1
        }
    elif command_exists wget; then
        wget -q -O "$needle_file" "$url" || {
            error "Download failed"
            exit 1
        }
    else
        error "Neither curl nor wget is available"
        exit 1
    fi

    success "Downloaded NEEDLE"

    # Create installation directory structure
    mkdir -p "$NEEDLE_INSTALL_DIR/bin"

    # Install the single-file binary
    info "Installing..."
    cp "$needle_file" "$NEEDLE_INSTALL_DIR/bin/needle"
    chmod +x "$NEEDLE_INSTALL_DIR/bin/needle"

    success "Installed NEEDLE to $NEEDLE_INSTALL_DIR"

    # Create symlink in bin directory
    mkdir -p "$NEEDLE_BIN_DIR"
    ln -sf "$NEEDLE_INSTALL_DIR/bin/needle" "$NEEDLE_BIN_DIR/needle"
    success "Created symlink: $NEEDLE_BIN_DIR/needle"

    # Verify installation
    if [[ -x "$NEEDLE_BIN_DIR/needle" ]]; then
        local installed_version
        installed_version=$("$NEEDLE_BIN_DIR/needle" version 2>/dev/null | head -1 || echo "unknown")
        success "Installation verified: $installed_version"
    fi

    # Handle PATH
    if ! in_path "$NEEDLE_BIN_DIR"; then
        if [[ "$NEEDLE_NO_MODIFY_PATH" != "true" ]]; then
            add_to_path "$NEEDLE_BIN_DIR"
        else
            warn "$NEEDLE_BIN_DIR is not in PATH"
            info "Add it manually: export PATH=\"$NEEDLE_BIN_DIR:\$PATH\""
        fi
    else
        success "$NEEDLE_BIN_DIR is already in PATH"
    fi

    # Show info about embedded agents
    echo ""
    info "Agent configurations are embedded in the binary"
    info "They will be extracted to ~/.needle/agents/ on first run"

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
    if [[ -f "$shell_rc" ]] && grep -q "needle.*PATH\|$dir" "$shell_rc" 2>/dev/null; then
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
        echo "export NEEDLE_HOME=\"$NEEDLE_INSTALL_DIR\""
    } >> "$shell_rc"

    success "Updated $shell_rc"
    info "Run 'source $shell_rc' or restart your shell to apply changes"
}

# Uninstall NEEDLE
uninstall_needle() {
    info "Uninstalling NEEDLE..."

    if $DRY_RUN; then
        info "[DRY RUN] Would remove $NEEDLE_INSTALL_DIR"
        info "[DRY RUN] Would remove symlink $NEEDLE_BIN_DIR/needle"
        return 0
    fi

    # Remove symlink
    if [[ -L "$NEEDLE_BIN_DIR/needle" ]]; then
        rm -f "$NEEDLE_BIN_DIR/needle"
        success "Removed symlink"
    fi

    # Remove installation directory
    if [[ -d "$NEEDLE_INSTALL_DIR" ]]; then
        rm -rf "$NEEDLE_INSTALL_DIR"
        success "Removed $NEEDLE_INSTALL_DIR"
    else
        warn "NEEDLE is not installed at $NEEDLE_INSTALL_DIR"
    fi

    # Optionally remove from PATH
    local shell_rc
    shell_rc=$(get_shell_rc)

    if [[ -f "$shell_rc" ]] && grep -q "NEEDLE" "$shell_rc" 2>/dev/null; then
        info "Removing NEEDLE entries from $shell_rc..."
        local temp_rc
        temp_rc=$(mktemp)
        grep -v "NEEDLE\|needle" "$shell_rc" > "$temp_rc" || true
        mv "$temp_rc" "$shell_rc"
        success "Cleaned up $shell_rc"
    fi

    success "NEEDLE has been uninstalled"
}

# -----------------------------------------------------------------------------
# CLI Argument Parsing
# -----------------------------------------------------------------------------

show_help() {
    cat << 'EOF'
NEEDLE Installer - One-liner installation for NEEDLE CLI

Usage:
  curl -fsSL https://raw.githubusercontent.com/anthropics/needle/main/scripts/install.sh | bash
  curl -fsSL https://raw.githubusercontent.com/anthropics/needle/main/scripts/install.sh | bash -s -- [OPTIONS]

Options:
  --version VERSION     Install specific version (default: latest)
  --install-dir DIR     Installation directory (default: ~/.needle)
  --non-interactive     Skip all prompts and use defaults
  --no-modify-path      Don't modify shell rc files
  --dry-run             Show what would be done without making changes
  --uninstall           Remove NEEDLE installation
  --help, -h            Show this help message

Environment Variables:
  NEEDLE_VERSION        Version to install (default: latest)
  NEEDLE_INSTALL_DIR    Installation directory (default: ~/.needle)
  NEEDLE_REPO           GitHub repository (default: anthropics/needle)
  NEEDLE_NO_MODIFY_PATH Don't modify PATH (true/false)

Examples:
  # Install latest version
  curl -fsSL https://raw.githubusercontent.com/anthropics/needle/main/scripts/install.sh | bash

  # Install specific version
  curl ... | bash -s -- --version 0.1.0

  # Install to custom directory
  curl ... | bash -s -- --install-dir ~/tools/needle

  # Non-interactive installation (for CI/CD)
  curl ... | bash -s -- --non-interactive

  # Uninstall
  curl ... | bash -s -- --uninstall

Installed Components:
  ~/.needle/              NEEDLE installation directory
  ├── bin/needle          Single-file self-contained CLI (with embedded configs)
  └── agents/             Agent configurations (extracted on first run)
      ├── claude-anthropic-sonnet.yaml
      ├── claude-anthropic-opus.yaml
      └── ...

  ~/.local/bin/needle     Symlink to bin/needle (added to PATH)

For more information, visit: https://github.com/anthropics/needle
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

    # Install
    echo ""
    install_needle

    # Show completion message
    echo ""
    success "NEEDLE installed successfully!"
    echo ""

    if in_path "$NEEDLE_BIN_DIR"; then
        printf '%bUsage:%b needle <command>\n' "$BOLD" "$NC"
        printf '%bExamples:%b\n' "$BOLD" "$NC"
        printf '  needle version         Show version information\n'
        printf '  needle agents          List available agent configurations\n'
        printf '  needle run             Start a worker\n'
        printf '  needle help            Show all available commands\n'
    else
        printf '%bTo use NEEDLE, add it to your PATH:%b\n' "$BOLD" "$NC"
        printf '  export PATH="%s:$PATH"\n' "$NEEDLE_BIN_DIR"
        printf '\nOr restart your shell to apply changes.\n'
    fi
}

# Run main
main "$@"
