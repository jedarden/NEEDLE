#!/usr/bin/env bash
# NEEDLE OS and Package Manager Detection Module
# Detects operating system, distribution, package manager, and architecture
# for cross-platform dependency installation support.

set -euo pipefail

# -----------------------------------------------------------------------------
# OS Detection
# -----------------------------------------------------------------------------

# Detect the operating system type
# Returns: linux, macos, windows (WSL/Cygwin/MINGW), or unknown
detect_os() {
    case "$(uname -s)" in
        Linux*)
            # Check if running under WSL
            if [[ -f /proc/version ]] && grep -qi microsoft /proc/version 2>/dev/null; then
                echo 'wsl'
            else
                echo 'linux'
            fi
            ;;
        Darwin*)
            echo 'macos'
            ;;
        MINGW*|MSYS*|CYGWIN*)
            echo 'windows'
            ;;
        *)
            echo 'unknown'
            ;;
    esac
}

# -----------------------------------------------------------------------------
# Linux Distribution Detection
# -----------------------------------------------------------------------------

# Detect the Linux distribution family
# Returns: debian, fedora, arch, suse, alpine, or unknown
# Note: Only meaningful when detect_os() returns 'linux' or 'wsl'
detect_distro() {
    local os
    os=$(detect_os)

    # Only detect distro on Linux systems
    if [[ "$os" != "linux" && "$os" != "wsl" ]]; then
        echo 'unknown'
        return
    fi

    # Check os-release first (most modern distros)
    if [[ -f /etc/os-release ]]; then
        source /etc/os-release 2>/dev/null || true

        case "${ID:-unknown}" in
            debian|ubuntu|linuxmint|pop|elementary|raspbian|kali|mx)
                echo 'debian'
                return
                ;;
            fedora|rhel|centos|rocky|almalinux|ol|scientific)
                echo 'fedora'
                return
                ;;
            arch|manjaro|endeavouros|garuda|arcolinux)
                echo 'arch'
                return
                ;;
            sles|opensuse|opensuse-leap|opensuse-tumbleweed)
                echo 'suse'
                return
                ;;
            alpine)
                echo 'alpine'
                return
                ;;
        esac

        # Check ID_LIKE for derivative distros
        case "${ID_LIKE:-}" in
            *debian*)
                echo 'debian'
                return
                ;;
            *fedora*|*rhel*|*centos*)
                echo 'fedora'
                return
                ;;
            *arch*)
                echo 'arch'
                return
                ;;
            *suse*)
                echo 'suse'
                return
                ;;
        esac
    fi

    # Fallback: check specific release files
    if [[ -f /etc/debian_version ]]; then
        echo 'debian'
        return
    fi

    if [[ -f /etc/fedora-release ]]; then
        echo 'fedora'
        return
    fi

    if [[ -f /etc/arch-release ]]; then
        echo 'arch'
        return
    fi

    if [[ -f /etc/SuSE-release ]] || [[ -f /etc/sles-release ]]; then
        echo 'suse'
        return
    fi

    if [[ -f /etc/alpine-release ]]; then
        echo 'alpine'
        return
    fi

    # Check for Red Hat/CentOS via /etc/redhat-release
    if [[ -f /etc/redhat-release ]]; then
        echo 'fedora'
        return
    fi

    echo 'unknown'
}

# Get the specific distribution name (more detailed than family)
# Returns: Distribution name (e.g., ubuntu, fedora, arch) or unknown
detect_distro_name() {
    local os
    os=$(detect_os)

    if [[ "$os" != "linux" && "$os" != "wsl" ]]; then
        echo 'unknown'
        return
    fi

    if [[ -f /etc/os-release ]]; then
        source /etc/os-release 2>/dev/null || true
        echo "${ID:-unknown}"
        return
    fi

    echo 'unknown'
}

# Get the distribution version
# Returns: Version string or unknown
detect_distro_version() {
    local os
    os=$(detect_os)

    if [[ "$os" != "linux" && "$os" != "wsl" ]]; then
        echo 'unknown'
        return
    fi

    if [[ -f /etc/os-release ]]; then
        source /etc/os-release 2>/dev/null || true
        echo "${VERSION_ID:-unknown}"
        return
    fi

    echo 'unknown'
}

# -----------------------------------------------------------------------------
# Package Manager Detection
# -----------------------------------------------------------------------------

# Detect the available package manager
# Returns: brew, apt, dnf, yum, pacman, zypper, apk, chocolatey, or manual
detect_pkg_manager() {
    # Check for Homebrew first (can be on macOS or Linux)
    if command -v brew &>/dev/null; then
        echo 'brew'
        return
    fi

    # Linux package managers
    if command -v apt-get &>/dev/null; then
        echo 'apt'
        return
    fi

    if command -v dnf &>/dev/null; then
        echo 'dnf'
        return
    fi

    if command -v yum &>/dev/null; then
        echo 'yum'
        return
    fi

    if command -v pacman &>/dev/null; then
        echo 'pacman'
        return
    fi

    if command -v zypper &>/dev/null; then
        echo 'zypper'
        return
    fi

    if command -v apk &>/dev/null; then
        echo 'apk'
        return
    fi

    # Windows package managers
    if command -v choco &>/dev/null; then
        echo 'chocolatey'
        return
    fi

    if command -v scoop &>/dev/null; then
        echo 'scoop'
        return
    fi

    # No known package manager found
    echo 'manual'
}

# Get the install command for a specific package manager
# Returns: The command prefix to install packages (e.g., "apt-get install -y")
get_install_command() {
    local pkg_manager="${1:-$(detect_pkg_manager)}"

    case "$pkg_manager" in
        brew)
            echo "brew install"
            ;;
        apt)
            echo "apt-get install -y"
            ;;
        dnf)
            echo "dnf install -y"
            ;;
        yum)
            echo "yum install -y"
            ;;
        pacman)
            echo "pacman -S --noconfirm"
            ;;
        zypper)
            echo "zypper install -y"
            ;;
        apk)
            echo "apk add"
            ;;
        chocolatey)
            echo "choco install -y"
            ;;
        scoop)
            echo "scoop install"
            ;;
        *)
            echo ""
            ;;
    esac
}

# Get the update command for a specific package manager
# Returns: The command to update package lists
get_update_command() {
    local pkg_manager="${1:-$(detect_pkg_manager)}"

    case "$pkg_manager" in
        brew)
            echo "brew update"
            ;;
        apt)
            echo "apt-get update"
            ;;
        dnf)
            echo "dnf makecache"
            ;;
        yum)
            echo "yum makecache"
            ;;
        pacman)
            echo "pacman -Sy"
            ;;
        zypper)
            echo "zypper refresh"
            ;;
        apk)
            echo "apk update"
            ;;
        *)
            echo ""
            ;;
    esac
}

# Check if a package is installed
# Returns: 0 if installed, 1 if not installed
pkg_is_installed() {
    local package="$1"
    local pkg_manager="${2:-$(detect_pkg_manager)}"

    case "$pkg_manager" in
        brew)
            brew list --formula "$package" &>/dev/null
            ;;
        apt)
            dpkg -l "$package" 2>/dev/null | grep -q "^ii"
            ;;
        dnf|yum)
            rpm -q "$package" &>/dev/null
            ;;
        pacman)
            pacman -Qi "$package" &>/dev/null
            ;;
        zypper)
            zypper se --installed-only "$package" &>/dev/null
            ;;
        apk)
            apk info -e "$package" &>/dev/null
            ;;
        chocolatey)
            choco list --local-only "$package" &>/dev/null
            ;;
        scoop)
            scoop list | grep -q "$package"
            ;;
        *)
            # Fallback: check if command exists
            command -v "$package" &>/dev/null
            ;;
    esac
}

# -----------------------------------------------------------------------------
# Architecture Detection
# -----------------------------------------------------------------------------

# Detect the CPU architecture for binary downloads
# Returns: amd64, arm64, armv7, or unknown
detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)
            echo 'amd64'
            ;;
        aarch64|arm64)
            echo 'arm64'
            ;;
        armv7l|armhf)
            echo 'armv7'
            ;;
        armv6l)
            echo 'armv6'
            ;;
        i386|i686)
            echo 'i386'
            ;;
        *)
            echo 'unknown'
            ;;
    esac
}

# Get the Go-style architecture name (commonly used in binary releases)
# Returns: amd64, arm64, etc. (Go-style naming)
detect_arch_go() {
    local arch
    arch=$(detect_arch)

    case "$arch" in
        amd64)
            echo 'amd64'
            ;;
        arm64)
            echo 'arm64'
            ;;
        armv7)
            echo 'arm'
            ;;
        armv6)
            echo 'arm'
            ;;
        i386)
            echo '386'
            ;;
        *)
            echo 'unknown'
            ;;
    esac
}

# Get the Rust-style target triple architecture
# Returns: x86_64, aarch64, etc.
detect_arch_rust() {
    case "$(uname -m)" in
        x86_64|amd64)
            echo 'x86_64'
            ;;
        aarch64|arm64)
            echo 'aarch64'
            ;;
        armv7l|armhf)
            echo 'armv7'
            ;;
        armv6l)
            echo 'arm'
            ;;
        i386|i686)
            echo 'i686'
            ;;
        *)
            echo 'unknown'
            ;;
    esac
}

# -----------------------------------------------------------------------------
# System Information Summary
# -----------------------------------------------------------------------------

# Get a comprehensive system information summary
# Returns: Human-readable system info
get_system_info() {
    local os distro distro_name distro_version pkg_manager arch

    os=$(detect_os)
    distro=$(detect_distro)
    distro_name=$(detect_distro_name)
    distro_version=$(detect_distro_version)
    pkg_manager=$(detect_pkg_manager)
    arch=$(detect_arch)

    cat <<EOF
System Information:
  OS:           ${os}
  Distro:       ${distro_name} (${distro}) ${distro_version}
  Package Mgr:  ${pkg_manager}
  Architecture: ${arch}
EOF
}

# Export all detected information as environment variables
# Sets: NEEDLE_OS, NEEDLE_DISTRO, NEEDLE_DISTRO_NAME, NEEDLE_PKG_MANAGER, NEEDLE_ARCH
export_system_info() {
    NEEDLE_OS=$(detect_os)
    NEEDLE_DISTRO=$(detect_distro)
    NEEDLE_DISTRO_NAME=$(detect_distro_name)
    NEEDLE_DISTRO_VERSION=$(detect_distro_version)
    NEEDLE_PKG_MANAGER=$(detect_pkg_manager)
    NEEDLE_ARCH=$(detect_arch)

    export NEEDLE_OS NEEDLE_DISTRO NEEDLE_DISTRO_NAME NEEDLE_DISTRO_VERSION NEEDLE_PKG_MANAGER NEEDLE_ARCH
}

# Print system info in JSON format (for scripting)
get_system_info_json() {
    local os distro distro_name distro_version pkg_manager arch

    os=$(detect_os)
    distro=$(detect_distro)
    distro_name=$(detect_distro_name)
    distro_version=$(detect_distro_version)
    pkg_manager=$(detect_pkg_manager)
    arch=$(detect_arch)

    cat <<EOF
{"os":"${os}","distro":"${distro}","distro_name":"${distro_name}","distro_version":"${distro_version}","pkg_manager":"${pkg_manager}","arch":"${arch}"}
EOF
}

# -----------------------------------------------------------------------------
# Utility Functions
# -----------------------------------------------------------------------------

# Check if running as root
is_root() {
    [[ $EUID -eq 0 ]]
}

# Check if sudo is available and needed
needs_sudo() {
    local pkg_manager="${1:-$(detect_pkg_manager)}"

    # Homebrew doesn't need sudo
    if [[ "$pkg_manager" == "brew" ]]; then
        return 1
    fi

    # Most system package managers need sudo if not root
    ! is_root
}

# Get sudo prefix for commands
get_sudo_prefix() {
    if needs_sudo; then
        echo "sudo"
    else
        echo ""
    fi
}

# Check if system is supported by NEEDLE
is_supported_system() {
    local os
    os=$(detect_os)

    case "$os" in
        linux|macos|wsl)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

# -----------------------------------------------------------------------------
# Main (for testing)
# -----------------------------------------------------------------------------

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    # Script is being run directly
    case "${1:-}" in
        --json)
            get_system_info_json
            ;;
        --export)
            export_system_info
            echo "Exported: NEEDLE_OS=$NEEDLE_OS NEEDLE_DISTRO=$NEEDLE_DISTRO NEEDLE_ARCH=$NEEDLE_ARCH"
            ;;
        --install-cmd)
            get_install_command "${2:-}"
            ;;
        --update-cmd)
            get_update_command "${2:-}"
            ;;
        --help|-h)
            echo "Usage: $(basename "$0") [OPTION]"
            echo ""
            echo "Options:"
            echo "  --json         Output system info as JSON"
            echo "  --export       Export system info as environment variables"
            echo "  --install-cmd  Print the package install command"
            echo "  --update-cmd   Print the package update command"
            echo "  --help, -h     Show this help message"
            echo ""
            echo "Without options, prints human-readable system information."
            ;;
        *)
            get_system_info
            ;;
    esac
fi
