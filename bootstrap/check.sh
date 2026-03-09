#!/usr/bin/env bash
# NEEDLE Dependency Detection Module
# Checks for required tools and validates their versions

set -euo pipefail

# -----------------------------------------------------------------------------
# Dependency Specification
# -----------------------------------------------------------------------------

# Minimum required versions for NEEDLE dependencies
# Only declare if not already set (allows for sourcing multiple times)
if [[ -z "${NEEDLE_DEPS_DECLARED:-}" ]]; then
    declare -gA NEEDLE_DEPS=(
        [tmux]="3.0"
        [jq]="1.6"
        [yq]="4.0"
        [br]="0.1"
        [claude]="1.0"
        [ubs]="1.0"
    )
    NEEDLE_DEPS_DECLARED=1
fi

# Human-readable names for dependencies
if [[ -z "${NEEDLE_DEPS_NAMES_DECLARED:-}" ]]; then
    declare -gA NEEDLE_DEPS_NAMES=(
        [tmux]="Terminal multiplexer"
        [jq]="JSON processor"
        [yq]="YAML processor"
        [br]="Beads queue manager"
        [claude]="Claude AI coding agent"
        [ubs]="Bug scanner"
    )
    NEEDLE_DEPS_NAMES_DECLARED=1
fi

# -----------------------------------------------------------------------------
# Version Parsing
# -----------------------------------------------------------------------------

# Parse version string from a command
# Arguments: $1 - command name
# Returns: Version string (e.g., "3.4" or "1.6")
_parse_dep_version() {
    local cmd="$1"
    local version=""

    case "$cmd" in
        tmux)
            # tmux -V outputs: tmux 3.4
            version=$(tmux -V 2>/dev/null | grep -oE '[0-9]+\.[0-9]+([a-z])?' | head -1)
            ;;
        jq)
            # jq --version outputs: jq-1.6
            version=$(jq --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+')
            ;;
        yq)
            # yq --version outputs: yq (https://github.com/mikefarah/yq/) version v4.44.3
            version=$(yq --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+')
            ;;
        br)
            # br --version outputs version info
            version=$(br --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+([.][0-9]+)?' | head -1)
            ;;
        claude)
            # claude --version outputs: 2.1.71 (Claude Code)
            version=$(claude --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+([.][0-9]+)?' | head -1)
            ;;
        ubs)
            # ubs --version outputs: ultimate_bug_scanner vX.X.X
            version=$(ubs --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+([.][0-9]+)?' | head -1)
            ;;
        *)
            # Generic fallback: try --version or -V
            version=$("$cmd" --version 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+([.][0-9]+)?' | head -1)
            if [[ -z "$version" ]]; then
                version=$("$cmd" -V 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+([.][0-9]+)?' | head -1)
            fi
            ;;
    esac

    echo "${version:-0.0}"
}

# -----------------------------------------------------------------------------
# Version Comparison
# -----------------------------------------------------------------------------

# Compare two version strings
# Returns: 0 if have >= need, 1 if have < need
# Handles semantic versioning correctly (e.g., 1.10 > 1.9)
_version_gte() {
    local have="$1"
    local need="$2"

    # If have is empty or 0.0, it's definitely not >=
    if [[ -z "$have" || "$have" == "0.0" ]]; then
        return 1
    fi

    # Use sort -V for version comparison (handles 1.10 > 1.9 correctly)
    # The oldest version will be first when sorted
    local sorted
    sorted=$(printf '%s\n%s\n' "$need" "$have" | sort -V | head -1)

    # If the minimum version is first or equal, we have >= minimum
    [[ "$sorted" == "$need" ]]
}

# Parse a version into components
# Sets: NEEDLE_VER_MAJOR, NEEDLE_VER_MINOR, NEEDLE_VER_PATCH
_parse_version_parts() {
    local version="$1"
    local major minor patch

    IFS='.' read -r major minor patch <<< "$version"

    NEEDLE_VER_MAJOR="${major:-0}"
    NEEDLE_VER_MINOR="${minor:-0}"
    NEEDLE_VER_PATCH="${patch:-0}"
}

# Compare two versions numerically (alternative implementation)
# Returns: -1 if v1 < v2, 0 if v1 == v2, 1 if v1 > v2
_compare_versions() {
    local v1="$1"
    local v2="$2"

    _parse_version_parts "$v1"
    local maj1="$NEEDLE_VER_MAJOR"
    local min1="$NEEDLE_VER_MINOR"
    local pat1="$NEEDLE_VER_PATCH"

    _parse_version_parts "$v2"
    local maj2="$NEEDLE_VER_MAJOR"
    local min2="$NEEDLE_VER_MINOR"
    local pat2="$NEEDLE_VER_PATCH"

    # Compare major
    if ((maj1 < maj2)); then echo "-1"; return; fi
    if ((maj1 > maj2)); then echo "1"; return; fi

    # Compare minor
    if ((min1 < min2)); then echo "-1"; return; fi
    if ((min1 > min2)); then echo "1"; return; fi

    # Compare patch
    if ((pat1 < pat2)); then echo "-1"; return; fi
    if ((pat1 > pat2)); then echo "1"; return; fi

    echo "0"
}

# -----------------------------------------------------------------------------
# Dependency Checking
# -----------------------------------------------------------------------------

# Check if a single dependency is installed
# Arguments: $1 - command name
# Returns: 0 if installed, 1 if not
_dep_is_installed() {
    local cmd="$1"
    command -v "$cmd" &>/dev/null
}

# Check a single dependency and return its status
# Arguments: $1 - command name
# Returns: "ok", "missing", or "outdated:have_version"
_check_single_dep() {
    local cmd="$1"
    local min_version="${NEEDLE_DEPS[$cmd]:-0.0}"

    # Check if command exists
    if ! _dep_is_installed "$cmd"; then
        echo "missing"
        return
    fi

    # Get installed version
    local have_version
    have_version=$(_parse_dep_version "$cmd")

    # Check version
    if _version_gte "$have_version" "$min_version"; then
        echo "ok"
    else
        echo "outdated:${have_version}"
    fi
}

# Check all dependencies
# Returns: 0 if all deps are ok, 1 if any are missing or outdated
# Sets global arrays: NEEDLE_MISSING_DEPS, NEEDLE_OUTDATED_DEPS, NEEDLE_OK_DEPS
_needle_check_deps() {
    NEEDLE_MISSING_DEPS=()
    NEEDLE_OUTDATED_DEPS=()
    NEEDLE_OK_DEPS=()

    local dep status have_version

    for dep in "${!NEEDLE_DEPS[@]}"; do
        status=$(_check_single_dep "$dep")

        case "$status" in
            ok)
                NEEDLE_OK_DEPS+=("$dep")
                ;;
            missing)
                NEEDLE_MISSING_DEPS+=("$dep")
                ;;
            outdated:*)
                have_version="${status#outdated:}"
                NEEDLE_OUTDATED_DEPS+=("$dep (have ${have_version}, need ${NEEDLE_DEPS[$dep]})")
                ;;
        esac
    done

    # Return success only if no missing or outdated deps
    [[ ${#NEEDLE_MISSING_DEPS[@]} -eq 0 && ${#NEEDLE_OUTDATED_DEPS[@]} -eq 0 ]]
}

# -----------------------------------------------------------------------------
# Reporting Functions
# -----------------------------------------------------------------------------

# Print dependency check results in human-readable format
# Arguments: $1 - if "quiet", only print errors
_print_dep_results() {
    local mode="${1:-normal}"
    local dep status have_version min_version

    # Print OK dependencies (unless quiet mode)
    if [[ "$mode" != "quiet" && ${#NEEDLE_OK_DEPS[@]} -gt 0 ]]; then
        for dep in "${NEEDLE_OK_DEPS[@]}"; do
            have_version=$(_parse_dep_version "$dep")
            min_version="${NEEDLE_DEPS[$dep]}"
            echo "  ✓ ${dep} ${have_version} (>= ${min_version})"
        done
    fi

    # Print outdated dependencies
    if [[ ${#NEEDLE_OUTDATED_DEPS[@]} -gt 0 ]]; then
        for entry in "${NEEDLE_OUTDATED_DEPS[@]}"; do
            echo "  ⚠ ${entry}" >&2
        done
    fi

    # Print missing dependencies
    if [[ ${#NEEDLE_MISSING_DEPS[@]} -gt 0 ]]; then
        for dep in "${NEEDLE_MISSING_DEPS[@]}"; do
            echo "  ✗ ${dep} not found (need ${NEEDLE_DEPS[$dep]})" >&2
        done
    fi
}

# Get dependency status as JSON (for scripting)
_get_deps_json() {
    local dep status have_version
    local first=true

    echo "{"
    for dep in "${!NEEDLE_DEPS[@]}"; do
        status=$(_check_single_dep "$dep")
        have_version=$(_parse_dep_version "$dep")

        if [[ "$first" == "true" ]]; then
            first=false
        else
            echo ","
        fi

        printf '  "%s": {"status": "%s", "version": "%s", "required": "%s"}' \
            "$dep" "${status%%:*}" "$have_version" "${NEEDLE_DEPS[$dep]}"
    done
    echo ""
    echo "}"
}

# Print install hints for missing dependencies
_print_install_hints() {
    local pkg_manager

    # Detect package manager using detect_os module if available
    if declare -f detect_pkg_manager &>/dev/null; then
        pkg_manager=$(detect_pkg_manager)
    else
        # Fallback detection
        if command -v brew &>/dev/null; then
            pkg_manager="brew"
        elif command -v apt-get &>/dev/null; then
            pkg_manager="apt"
        elif command -v dnf &>/dev/null; then
            pkg_manager="dnf"
        elif command -v pacman &>/dev/null; then
            pkg_manager="pacman"
        else
            pkg_manager="manual"
        fi
    fi

    echo ""
    echo "Install missing dependencies:"

    local dep
    for dep in "${NEEDLE_MISSING_DEPS[@]}"; do
        case "$dep" in
            tmux)
                case "$pkg_manager" in
                    brew)   echo "  brew install tmux" ;;
                    apt)    echo "  sudo apt-get install tmux" ;;
                    dnf)    echo "  sudo dnf install tmux" ;;
                    pacman) echo "  sudo pacman -S tmux" ;;
                    *)      echo "  Install tmux 3.0+ from your package manager" ;;
                esac
                ;;
            jq)
                case "$pkg_manager" in
                    brew)   echo "  brew install jq" ;;
                    apt)    echo "  sudo apt-get install jq" ;;
                    dnf)    echo "  sudo dnf install jq" ;;
                    pacman) echo "  sudo pacman -S jq" ;;
                    *)      echo "  Install jq 1.6+ from your package manager" ;;
                esac
                ;;
            yq)
                case "$pkg_manager" in
                    brew)   echo "  brew install yq" ;;
                    apt)    echo "  sudo apt-get install yq (or download from GitHub)" ;;
                    dnf)    echo "  sudo dnf install yq (or download from GitHub)" ;;
                    pacman) echo "  sudo pacman -S yq" ;;
                    *)      echo "  Install yq 4.0+ from https://github.com/mikefarah/yq" ;;
                esac
                ;;
            br)
                echo "  Install br from https://github.com/anthropics/bead-runner"
                ;;
            claude)
                echo "  Install Claude Code: npm install -g @anthropic-ai/claude-code"
                echo "  Or visit: https://claude.ai/code"
                ;;
            ubs)
                echo "  Install ultimate_bug_scanner (UBS):"
                echo "    curl -fsSL https://raw.githubusercontent.com/Dicklesworthstone/ultimate_bug_scanner/master/install.sh | bash"
                echo "  Or visit: https://github.com/Dicklesworthstone/ultimate_bug_scanner"
                ;;
        esac
    done
}

# -----------------------------------------------------------------------------
# Main Check Function
# -----------------------------------------------------------------------------

# Run full dependency check with output
# Arguments: $1 - "quiet" to suppress OK messages
# Returns: 0 if all deps ok, 1 if issues found
needle_check_deps() {
    local quiet="${1:-}"

    echo "Checking NEEDLE dependencies..."

    if _needle_check_deps; then
        _print_dep_results "$quiet"
        echo ""
        echo "All dependencies satisfied!"
        return 0
    else
        _print_dep_results "$quiet"

        if [[ ${#NEEDLE_MISSING_DEPS[@]} -gt 0 ]]; then
            _print_install_hints
        fi

        return 1
    fi
}

# Quick check without output (for scripting)
# Returns: 0 if all deps ok, 1 if issues found
needle_deps_ok() {
    _needle_check_deps &>/dev/null || return 1
}

# Get list of missing dependencies
needle_missing_deps() {
    _needle_check_deps &>/dev/null || true
    printf '%s\n' "${NEEDLE_MISSING_DEPS[@]}"
}

# Get list of outdated dependencies
needle_outdated_deps() {
    _needle_check_deps &>/dev/null || true
    printf '%s\n' "${NEEDLE_OUTDATED_DEPS[@]}"
}

# -----------------------------------------------------------------------------
# Main (for direct execution)
# -----------------------------------------------------------------------------

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    # Script is being run directly
    case "${1:-}" in
        --json)
            _needle_check_deps &>/dev/null || true
            _get_deps_json
            ;;
        --quiet|-q)
            if _needle_check_deps &>/dev/null; then
                exit 0
            else
                _print_dep_results "quiet" || true
                exit 1
            fi
            ;;
        --missing)
            _needle_check_deps &>/dev/null || true
            printf '%s\n' "${NEEDLE_MISSING_DEPS[@]}"
            ;;
        --outdated)
            _needle_check_deps &>/dev/null || true
            printf '%s\n' "${NEEDLE_OUTDATED_DEPS[@]}"
            ;;
        --help|-h)
            echo "Usage: $(basename "$0") [OPTION]"
            echo ""
            echo "Options:"
            echo "  --json      Output dependency status as JSON"
            echo "  --quiet,-q  Only print errors, exit code indicates status"
            echo "  --missing   List missing dependencies"
            echo "  --outdated  List outdated dependencies"
            echo "  --help,-h   Show this help message"
            echo ""
            echo "Without options, prints human-readable dependency check."
            echo ""
            echo "Exit codes:"
            echo "  0 - All dependencies satisfied"
            echo "  1 - Missing or outdated dependencies"
            ;;
        *)
            needle_check_deps
            exit $?
            ;;
    esac
fi
