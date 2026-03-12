#!/usr/bin/env bash
# NEEDLE CLI Update Check Module
# Non-blocking version check on startup with 24h cache

# Cache file for version check results
NEEDLE_VERSION_CACHE_FILE="$NEEDLE_HOME/$NEEDLE_CACHE_DIR/version_check.json"

# Default configuration values
NEEDLE_UPDATE_CHECK_TTL="${NEEDLE_UPDATE_CHECK_TTL:-86400}"  # 24 hours in seconds
NEEDLE_UPDATE_CHECK_TIMEOUT="${NEEDLE_UPDATE_CHECK_TIMEOUT:-5}"  # 5 seconds timeout for network

# -----------------------------------------------------------------------------
# Version Check Functions
# -----------------------------------------------------------------------------

# Check if update check is enabled
# Returns 0 if enabled, 1 if disabled
_needle_update_check_enabled() {
    # Check environment variable override
    if [[ "${NEEDLE_NO_UPDATE_CHECK:-false}" == "true" ]]; then
        return 1
    fi

    # Check config file if available
    if _needle_is_initialized 2>/dev/null; then
        local enabled
        enabled=$(get_config "updates.check_on_startup" "true" 2>/dev/null)
        if [[ "$enabled" == "false" ]]; then
            return 1
        fi
    fi

    return 0
}

# Get cache age in seconds
# Returns 0 if cache doesn't exist
_needle_get_cache_age() {
    local cache_file="$NEEDLE_VERSION_CACHE_FILE"

    if [[ ! -f "$cache_file" ]]; then
        echo "0"
        return
    fi

    local now=$(date +%s)
    local cache_mtime=$(stat -c %Y "$cache_file" 2>/dev/null || stat -f %m "$cache_file" 2>/dev/null)

    if [[ -z "$cache_mtime" ]] || [[ ! "$cache_mtime" =~ ^[0-9]+$ ]]; then
        echo "0"
        return
    fi

    echo $((now - cache_mtime))
}

# Check if cache is still valid (within TTL)
_needle_cache_valid() {
    local ttl="${1:-$NEEDLE_UPDATE_CHECK_TTL}"
    local age=$(_needle_get_cache_age)

    [[ "$age" -lt "$ttl" ]]
}

# Read cached version info
# Returns: JSON with latest_version, current_version, update_available, checked_at
_needle_read_cache() {
    local cache_file="$NEEDLE_VERSION_CACHE_FILE"

    if [[ -f "$cache_file" ]]; then
        cat "$cache_file" 2>/dev/null
    else
        echo "{}"
    fi
}

# Write version info to cache
_needle_write_cache() {
    local latest_version="$1"
    local update_available="$2"
    local changelog_url="$3"
    local cache_file="$NEEDLE_VERSION_CACHE_FILE"
    local cache_dir
    cache_dir=$(dirname "$cache_file")

    # Ensure cache directory exists
    mkdir -p "$cache_dir" 2>/dev/null || return 1

    # Write JSON cache
    cat > "$cache_file" << EOF
{
  "current_version": "$NEEDLE_VERSION",
  "latest_version": "$latest_version",
  "update_available": $update_available,
  "changelog_url": "$changelog_url",
  "checked_at": "$(date -Iseconds 2>/dev/null || date +%Y-%m-%dT%H:%M:%S)"
}
EOF
}

# Fetch latest version from GitHub (with timeout)
# Returns: version string (e.g., "0.2.0") or empty on failure
_needle_fetch_latest_version() {
    local api_url="$NEEDLE_GITHUB_API/repos/$NEEDLE_GITHUB_REPO/releases/latest"
    local response
    local version

    # Use curl or wget with timeout
    if _needle_command_exists curl; then
        response=$(curl -sf --connect-timeout "$NEEDLE_UPDATE_CHECK_TIMEOUT" \
            --max-time $((NEEDLE_UPDATE_CHECK_TIMEOUT * 2)) \
            "$api_url" 2>/dev/null)
    elif _needle_command_exists wget; then
        response=$(wget -qO- --timeout="$NEEDLE_UPDATE_CHECK_TIMEOUT" \
            "$api_url" 2>/dev/null)
    else
        return 1
    fi

    if [[ -z "$response" ]]; then
        return 1
    fi

    # Extract tag_name (e.g., "v0.1.0") and strip 'v' prefix
    version=$(echo "$response" | grep -m1 '"tag_name"' | \
        sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/' | \
        sed 's/^v//')

    if [[ -z "$version" ]]; then
        return 1
    fi

    echo "$version"
}

# Build changelog URL for a version
_needle_get_changelog_url() {
    local version="$1"
    echo "https://github.com/$NEEDLE_GITHUB_REPO/releases/tag/v${version}"
}

# Perform background version check (non-blocking)
# This updates the cache file in the background
_needle_background_version_check() {
    # Skip if disabled
    if ! _needle_update_check_enabled; then
        return 0
    fi

    # Skip if cache is still valid
    if _needle_cache_valid; then
        return 0
    fi

    # Run check in background
    (
        # Source required modules in subshell
        source "$NEEDLE_ROOT_DIR/src/lib/constants.sh" 2>/dev/null || true
        source "$NEEDLE_ROOT_DIR/src/lib/output.sh" 2>/dev/null || true
        source "$NEEDLE_ROOT_DIR/src/lib/utils.sh" 2>/dev/null || true

        local latest
        latest=$(_needle_fetch_latest_version 2>/dev/null)

        if [[ -n "$latest" ]]; then
            local update_available="false"
            local changelog_url=""

            # Compare versions
            if _needle_version_compare "$latest" "$NEEDLE_VERSION" 2>/dev/null; then
                update_available="true"
                changelog_url=$(_needle_get_changelog_url "$latest")
            fi

            _needle_write_cache "$latest" "$update_available" "$changelog_url" 2>/dev/null
        fi
    ) &>/dev/null &

    # Don't wait for background process
    disown 2>/dev/null || true
}

# Check for updates and display notification
# This is the main entry point for startup version checks
# It's non-blocking: uses cached data if available, triggers background refresh if stale
_needle_check_updates_on_startup() {
    # Skip if disabled
    if ! _needle_update_check_enabled; then
        return 0
    fi

    # Skip for certain commands (init, version, upgrade, help)
    case "${1:-}" in
        init|version|-V|upgrade|rollback|help|--help|-h|completion|setup|_run_worker)
            return 0
            ;;
    esac

    # Skip if in quiet mode
    if [[ "${NEEDLE_QUIET:-}" == "true" ]]; then
        return 0
    fi

    local cache
    local latest_version=""
    local update_available="false"
    local changelog_url=""

    # Read cache
    cache=$(_needle_read_cache)

    if [[ -n "$cache" ]] && [[ "$cache" != "{}" ]]; then
        # Parse cache using basic string extraction (works without jq)
        latest_version=$(echo "$cache" | grep -o '"latest_version"[[:space:]]*:[[:space:]]*"[^"]*"' | \
            sed 's/.*: *"\([^"]*\)".*/\1/')
        update_available=$(echo "$cache" | grep -o '"update_available"[[:space:]]*:[[:space:]]*[a-z]*' | \
            sed 's/.*: *//')
        changelog_url=$(echo "$cache" | grep -o '"changelog_url"[[:space:]]*:[[:space:]]*"[^"]*"' | \
            sed 's/.*: *"\([^"]*\)".*/\1/')
    fi

    # Trigger background check if cache is stale (but don't wait)
    if ! _needle_cache_valid; then
        _needle_background_version_check
    fi

    # Display notification if update is available
    if [[ "$update_available" == "true" ]] && [[ -n "$latest_version" ]]; then
        _needle_print ""
        _needle_print_color "$NEEDLE_COLOR_YELLOW" "    NEEDLE v$NEEDLE_VERSION → v$latest_version available"
        if [[ -n "$changelog_url" ]]; then
            _needle_print_color "$NEEDLE_COLOR_DIM" "    Run 'needle upgrade' to update (changelog: $changelog_url)"
        else
            _needle_print_color "$NEEDLE_COLOR_DIM" "    Run 'needle upgrade' to update"
        fi
        _needle_print ""
    fi

    return 0
}

# Force a synchronous version check (used by upgrade command)
# Returns 0 if update available, 1 if not, 2 on error
_needle_check_updates_sync() {
    local latest
    latest=$(_needle_fetch_latest_version)

    if [[ -z "$latest" ]]; then
        return 2
    fi

    local update_available="false"

    if _needle_version_compare "$latest" "$NEEDLE_VERSION"; then
        update_available="true"
    fi

    local changelog_url=$(_needle_get_changelog_url "$latest")

    # Update cache
    _needle_write_cache "$latest" "$update_available" "$changelog_url"

    if [[ "$update_available" == "true" ]]; then
        return 0
    else
        return 1
    fi
}

# Get cached latest version (for scripting)
_needle_get_cached_latest_version() {
    local cache
    cache=$(_needle_read_cache)

    if [[ -n "$cache" ]] && [[ "$cache" != "{}" ]]; then
        echo "$cache" | grep -o '"latest_version"[[:space:]]*:[[:space:]]*"[^"]*"' | \
            sed 's/.*: *"\([^"]*\)".*/\1/'
    fi
}
