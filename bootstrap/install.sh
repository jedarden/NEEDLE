#!/usr/bin/env bash
# NEEDLE Dependency Installation Module
# Automatically installs missing dependencies using package managers or binary downloads

set -euo pipefail

# -----------------------------------------------------------------------------
# Module Dependencies
# -----------------------------------------------------------------------------

# Get the directory where this script is located
NEEDLE_BOOTSTRAP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Source required modules
source "$NEEDLE_BOOTSTRAP_DIR/detect_os.sh"
source "$NEEDLE_BOOTSTRAP_DIR/check.sh"

# Source paths module if available
if [[ -f "$NEEDLE_BOOTSTRAP_DIR/../src/lib/paths.sh" ]]; then
    source "$NEEDLE_BOOTSTRAP_DIR/../src/lib/paths.sh"
fi

# Source output utilities if available
if [[ -f "$NEEDLE_BOOTSTRAP_DIR/../src/lib/output.sh" ]]; then
    source "$NEEDLE_BOOTSTRAP_DIR/../src/lib/output.sh"
fi

# Source constants if available
if [[ -f "$NEEDLE_BOOTSTRAP_DIR/../src/lib/constants.sh" ]]; then
    source "$NEEDLE_BOOTSTRAP_DIR/../src/lib/constants.sh"
fi

# -----------------------------------------------------------------------------
# Installation State
# -----------------------------------------------------------------------------

# Track what was installed in this session
NEEDLE_INSTALLED_DEPS=()
NEEDLE_FAILED_DEPS=()
NEEDLE_SKIPPED_DEPS=()

# -----------------------------------------------------------------------------
# Path Management
# -----------------------------------------------------------------------------

# Ensure cache directory exists
_needle_ensure_cache_dir() {
    local cache_dir
    cache_dir=$(_needle_get_cache_dir)
    mkdir -p "$cache_dir"
    echo "$cache_dir"
}

# Get cache directory path
_needle_get_cache_dir() {
    # If NEEDLE_CACHE_DIR is set and is an absolute path, use it
    # Otherwise, combine with NEEDLE_HOME or default to ~/.needle/cache
    if [[ -n "${NEEDLE_CACHE_DIR:-}" ]]; then
        if [[ "$NEEDLE_CACHE_DIR" == /* ]]; then
            echo "$NEEDLE_CACHE_DIR"
        else
            echo "${NEEDLE_HOME:-$HOME/.needle}/$NEEDLE_CACHE_DIR"
        fi
    else
        echo "$HOME/.needle/cache"
    fi
}

# Add a directory to PATH if not already present
_needle_add_to_path() {
    local dir="$1"

    # Check if already in PATH
    if [[ ":$PATH:" == *":$dir:"* ]]; then
        return 0
    fi

    export PATH="$dir:$PATH"
}

# Setup PATH for NEEDLE cache binaries
_needle_setup_path() {
    local cache_dir
    cache_dir=$(_needle_get_cache_dir)

    # Add cache dir to PATH
    _needle_add_to_path "$cache_dir"

    # Also add ~/.local/bin if it exists
    if [[ -d "$HOME/.local/bin" ]]; then
        _needle_add_to_path "$HOME/.local/bin"
    fi
}

# Detect shell rc file
_needle_detect_shell_rc() {
    local shell_name=""
    local rc_file=""

    # Get current shell
    if [[ -n "${SHELL:-}" ]]; then
        shell_name=$(basename "$SHELL")
    fi

    # Check for shell-specific rc files
    case "$shell_name" in
        zsh)
            rc_file="$HOME/.zshrc"
            ;;
        bash)
            rc_file="$HOME/.bashrc"
            ;;
        fish)
            rc_file="$HOME/.config/fish/config.fish"
            ;;
        *)
            # Fallback order
            if [[ -f "$HOME/.bashrc" ]]; then
                rc_file="$HOME/.bashrc"
            elif [[ -f "$HOME/.zshrc" ]]; then
                rc_file="$HOME/.zshrc"
            elif [[ -f "$HOME/.profile" ]]; then
                rc_file="$HOME/.profile"
            fi
            ;;
    esac

    echo "${rc_file:-}"
}

# Add directory to shell rc file for persistence
_needle_add_to_shell_rc() {
    local dir="$1"
    local rc_file
    rc_file=$(_needle_detect_shell_rc)

    if [[ -z "$rc_file" || ! -f "$rc_file" ]]; then
        return 1
    fi

    # Check if already in rc file
    if grep -qF "export PATH=\"\$PATH:$dir\"" "$rc_file" 2>/dev/null || \
       grep -qF "export PATH=\"$dir:\$PATH\"" "$rc_file" 2>/dev/null; then
        return 0
    fi

    # Add to rc file with comment
    {
        echo ""
        echo "# Added by NEEDLE"
        echo "export PATH=\"\$PATH:$dir\""
    } >> "$rc_file"

    return 0
}

# -----------------------------------------------------------------------------
# Download Utilities
# -----------------------------------------------------------------------------

# Download a file with progress
_needle_download() {
    local url="$1"
    local output="$2"

    # Use curl or wget
    if command -v curl &>/dev/null; then
        curl -fsSL "$url" -o "$output"
    elif command -v wget &>/dev/null; then
        wget -q "$url" -O "$output"
    else
        return 1
    fi
}

# Get OS name for binary downloads
_needle_get_binary_os() {
    local os
    os=$(detect_os)

    case "$os" in
        linux|wsl) echo "linux" ;;
        macos)     echo "darwin" ;;
        windows)   echo "windows" ;;
        *)         echo "linux" ;;
    esac
}

# Get architecture for binary downloads
_needle_get_binary_arch() {
    detect_arch
}

# -----------------------------------------------------------------------------
# Package Manager Installation
# -----------------------------------------------------------------------------

# Run package manager update
_needle_pkg_update() {
    local pkg_manager
    pkg_manager=$(detect_pkg_manager)

    local update_cmd
    update_cmd=$(get_update_command "$pkg_manager")

    if [[ -z "$update_cmd" ]]; then
        return 0
    fi

    local sudo_prefix=""
    if needs_sudo "$pkg_manager"; then
        sudo_prefix="sudo"
    fi

    $sudo_prefix $update_cmd
}

# Install package via package manager
_needle_pkg_install() {
    local package="$1"
    local pkg_manager
    pkg_manager=$(detect_pkg_manager)

    local install_cmd
    install_cmd=$(get_install_command "$pkg_manager")

    if [[ -z "$install_cmd" ]]; then
        return 1
    fi

    local sudo_prefix=""
    if needs_sudo "$pkg_manager"; then
        sudo_prefix="sudo"
    fi

    $sudo_prefix $install_cmd "$package"
}

# -----------------------------------------------------------------------------
# Dependency Installation Functions
# -----------------------------------------------------------------------------

# Install tmux
_needle_install_tmux() {
    local pkg_manager
    pkg_manager=$(detect_pkg_manager)

    echo "Installing tmux..."

    case "$pkg_manager" in
        apt)
            sudo apt-get update && sudo apt-get install -y tmux
            ;;
        dnf)
            sudo dnf install -y tmux
            ;;
        yum)
            sudo yum install -y tmux
            ;;
        pacman)
            sudo pacman -S --noconfirm tmux
            ;;
        zypper)
            sudo zypper install -y tmux
            ;;
        apk)
            sudo apk add tmux
            ;;
        brew)
            brew install tmux
            ;;
        *)
            echo "Cannot auto-install tmux with $pkg_manager. Please install manually." >&2
            return 1
            ;;
    esac
}

# Install jq
_needle_install_jq() {
    local pkg_manager
    pkg_manager=$(detect_pkg_manager)

    echo "Installing jq..."

    case "$pkg_manager" in
        apt)
            sudo apt-get install -y jq
            ;;
        dnf)
            sudo dnf install -y jq
            ;;
        yum)
            sudo yum install -y jq
            ;;
        pacman)
            sudo pacman -S --noconfirm jq
            ;;
        zypper)
            sudo zypper install -y jq
            ;;
        apk)
            sudo apk add jq
            ;;
        brew)
            brew install jq
            ;;
        *)
            # Fallback: download binary
            echo "Downloading jq binary..."
            local cache_dir
            cache_dir=$(_needle_ensure_cache_dir)
            local os arch url

            os=$(_needle_get_binary_os)
            arch=$(_needle_get_binary_arch)

            # jq binary naming
            case "$os-$arch" in
                linux-amd64|darwin-amd64)
                    url="https://github.com/jqlang/jq/releases/latest/download/jq-${os}-amd64"
                    ;;
                linux-arm64|darwin-arm64)
                    url="https://github.com/jqlang/jq/releases/latest/download/jq-${os}-arm64"
                    ;;
                *)
                    echo "Cannot auto-install jq for $os-$arch. Please install manually." >&2
                    return 1
                    ;;
            esac

            _needle_download "$url" "$cache_dir/jq" || {
                echo "Failed to download jq binary" >&2
                return 1
            }
            chmod +x "$cache_dir/jq"
            ;;
    esac
}

# Install yq
_needle_install_yq() {
    local pkg_manager
    pkg_manager=$(detect_pkg_manager)

    echo "Installing yq..."

    case "$pkg_manager" in
        brew)
            brew install yq
            ;;
        pacman)
            sudo pacman -S --noconfirm yq
            ;;
        *)
            # yq is often not in package managers, download binary
            echo "Downloading yq binary..."
            local cache_dir
            cache_dir=$(_needle_ensure_cache_dir)
            local os arch url

            os=$(_needle_get_binary_os)
            arch=$(_needle_get_binary_arch)

            # yq binary naming
            case "$arch" in
                amd64)
                    url="https://github.com/mikefarah/yq/releases/latest/download/yq_${os}_amd64"
                    ;;
                arm64)
                    url="https://github.com/mikefarah/yq/releases/latest/download/yq_${os}_arm64"
                    ;;
                armv7|armv6)
                    url="https://github.com/mikefarah/yq/releases/latest/download/yq_${os}_arm"
                    ;;
                *)
                    echo "Cannot auto-install yq for $os-$arch. Please install manually." >&2
                    return 1
                    ;;
            esac

            _needle_download "$url" "$cache_dir/yq" || {
                echo "Failed to download yq binary" >&2
                return 1
            }
            chmod +x "$cache_dir/yq"
            ;;
    esac
}

# Install br (beads runner)
_needle_install_br() {
    echo "Installing br..."

    local cache_dir
    cache_dir=$(_needle_ensure_cache_dir)
    local os arch url

    os=$(_needle_get_binary_os)
    arch=$(_needle_get_binary_arch)

    # br binary naming from beads_rust releases
    case "$arch" in
        amd64)
            url="https://github.com/Dicklesworthstone/beads_rust/releases/latest/download/br-${os}-x86_64"
            ;;
        arm64)
            url="https://github.com/Dicklesworthstone/beads_rust/releases/latest/download/br-${os}-aarch64"
            ;;
        *)
            echo "Cannot auto-install br for $os-$arch. Please install manually from:" >&2
            echo "  https://github.com/Dicklesworthstone/beads_rust/releases" >&2
            return 1
            ;;
    esac

    _needle_download "$url" "$cache_dir/br" || {
        echo "Failed to download br binary. Please install manually from:" >&2
        echo "  https://github.com/Dicklesworthstone/beads_rust/releases" >&2
        return 1
    }
    chmod +x "$cache_dir/br"
}

# Generic installer dispatcher
_needle_install_dep() {
    local dep="$1"

    case "$dep" in
        tmux)
            _needle_install_tmux
            ;;
        jq)
            _needle_install_jq
            ;;
        yq)
            _needle_install_yq
            ;;
        br)
            _needle_install_br
            ;;
        *)
            echo "Unknown dependency: $dep" >&2
            return 1
            ;;
    esac
}

# -----------------------------------------------------------------------------
# Main Installation Functions
# -----------------------------------------------------------------------------

# Install a single dependency with verification
needle_install_dep() {
    local dep="$1"
    local force="${2:-false}"

    # Check if already installed (unless forced)
    if [[ "$force" != "true" ]]; then
        if _dep_is_installed "$dep"; then
            local version
            version=$(_parse_dep_version "$dep")
            echo "  $dep $version already installed"
            NEEDLE_SKIPPED_DEPS+=("$dep")
            return 0
        fi
    fi

    # Install
    if _needle_install_dep "$dep"; then
        # Verify installation
        if _dep_is_installed "$dep"; then
            local version
            version=$(_parse_dep_version "$dep")
            echo "  Installed $dep $version"
            NEEDLE_INSTALLED_DEPS+=("$dep")
            return 0
        else
            echo "  Installation appeared successful but $dep not found in PATH" >&2
            NEEDLE_FAILED_DEPS+=("$dep")
            return 1
        fi
    else
        NEEDLE_FAILED_DEPS+=("$dep")
        return 1
    fi
}

# Install all missing dependencies
needle_install_missing() {
    local auto="${1:-true}"

    # Reset state
    NEEDLE_INSTALLED_DEPS=()
    NEEDLE_FAILED_DEPS=()
    NEEDLE_SKIPPED_DEPS=()

    # Check current dependency status
    _needle_check_deps || true

    # If no missing deps, we're done
    if [[ ${#NEEDLE_MISSING_DEPS[@]} -eq 0 ]]; then
        echo "All dependencies are already installed."
        return 0
    fi

    echo "Missing dependencies: ${NEEDLE_MISSING_DEPS[*]}"
    echo ""

    # If not auto, prompt for confirmation
    if [[ "$auto" != "true" ]]; then
        echo "Install missing dependencies? [Y/n]"
        read -r response
        case "$response" in
            n*|N*)
                echo "Installation cancelled."
                return 1
                ;;
        esac
    fi

    # Setup PATH for binary downloads
    _needle_setup_path

    # Install each missing dependency
    for dep in "${NEEDLE_MISSING_DEPS[@]}"; do
        needle_install_dep "$dep" || true
    done

    # Update PATH if we installed anything to cache
    if [[ ${#NEEDLE_INSTALLED_DEPS[@]} -gt 0 ]]; then
        local cache_dir
        cache_dir=$(_needle_get_cache_dir)

        # Add to current PATH
        _needle_add_to_path "$cache_dir"

        # Add to shell rc for persistence
        _needle_add_to_shell_rc "$cache_dir" || true
    fi

    # Summary
    echo ""
    echo "Installation Summary:"
    echo "  Installed: ${#NEEDLE_INSTALLED_DEPS[@]}"
    echo "  Skipped:   ${#NEEDLE_SKIPPED_DEPS[@]}"
    echo "  Failed:    ${#NEEDLE_FAILED_DEPS[@]}"

    if [[ ${#NEEDLE_FAILED_DEPS[@]} -gt 0 ]]; then
        echo ""
        echo "Failed to install: ${NEEDLE_FAILED_DEPS[*]}"
        return 1
    fi

    return 0
}

# Bootstrap all dependencies
needle_bootstrap() {
    local auto="${1:-true}"

    echo "NEEDLE Dependency Bootstrap"
    echo "==========================="
    echo ""

    # Check system support
    if ! is_supported_system; then
        echo "Error: Unsupported system. NEEDLE requires Linux, macOS, or WSL." >&2
        return 1
    fi

    # Show system info
    echo "System Information:"
    echo "  OS:           $(detect_os)"
    echo "  Distro:       $(detect_distro_name) ($(detect_distro))"
    echo "  Package Mgr:  $(detect_pkg_manager)"
    echo "  Architecture: $(detect_arch)"
    echo ""

    # Install missing dependencies
    needle_install_missing "$auto"
}

# -----------------------------------------------------------------------------
# Main (for direct execution)
# -----------------------------------------------------------------------------

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    # Script is being run directly
    case "${1:-}" in
        --all|--bootstrap)
            needle_bootstrap "${2:-true}"
            ;;
        --missing)
            needle_install_missing "${2:-true}"
            ;;
        --dep)
            if [[ -z "${2:-}" ]]; then
                echo "Error: Missing dependency name" >&2
                echo "Usage: $(basename "$0") --dep <name>" >&2
                exit 1
            fi
            needle_install_dep "$2" "${3:-false}"
            ;;
        --list)
            echo "Available dependencies:"
            for dep in "${!NEEDLE_DEPS[@]}"; do
                echo "  $dep (${NEEDLE_DEPS_NAMES[$dep]:-$dep}) - minimum version ${NEEDLE_DEPS[$dep]}"
            done
            ;;
        --check)
            needle_check_deps
            ;;
        --help|-h)
            echo "Usage: $(basename "$0") [COMMAND]"
            echo ""
            echo "NEEDLE Dependency Installation Module"
            echo ""
            echo "Commands:"
            echo "  --all, --bootstrap    Install all missing dependencies (default)"
            echo "  --missing             Install only missing dependencies"
            echo "  --dep <name>          Install a specific dependency"
            echo "  --list                List available dependencies"
            echo "  --check               Check dependency status"
            echo "  --help, -h            Show this help message"
            echo ""
            echo "Examples:"
            echo "  $(basename "$0")              # Bootstrap all dependencies"
            echo "  $(basename "$0") --dep yq     # Install only yq"
            echo "  $(basename "$0") --check      # Check current status"
            ;;
        *)
            needle_bootstrap
            ;;
    esac
fi
