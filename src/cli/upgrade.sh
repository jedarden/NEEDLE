#!/usr/bin/env bash
# NEEDLE CLI Upgrade Subcommand
# Self-update functionality with rollback support

# Upgrade constants
NEEDLE_GITHUB_REPO="${NEEDLE_GITHUB_REPO:-coder/needle}"
NEEDLE_GITHUB_API="${NEEDLE_GITHUB_API:-https://api.github.com}"
NEEDLE_UPGRADE_DIR="$NEEDLE_HOME/upgrade"
NEEDLE_BACKUP_DIR="$NEEDLE_UPGRADE_DIR/backups"
NEEDLE_DOWNLOAD_DIR="$NEEDLE_UPGRADE_DIR/downloads"
NEEDLE_MAX_BACKUPS="${NEEDLE_MAX_BACKUPS:-5}"

# -----------------------------------------------------------------------------
# Help
# -----------------------------------------------------------------------------

_needle_upgrade_help() {
    _needle_print "Check for and install NEEDLE updates

Downloads and installs the latest version of NEEDLE from GitHub
releases with automatic backup and rollback support.

USAGE:
    needle upgrade [OPTIONS]

OPTIONS:
    -c, --check              Check for updates without installing
    -y, --yes                Upgrade without confirmation
    -f, --force              Reinstall even if already latest
    -l, --list               List available versions

    --version <VERSION>      Install specific version (e.g., \"1.2.0\")
    --local <PATH>           Install from local file (air-gapped)

    -h, --help               Print help information

EXAMPLES:
    # Check for updates
    needle upgrade --check

    # Upgrade to latest
    needle upgrade

    # Upgrade without prompts
    needle upgrade --yes

    # Install specific version
    needle upgrade --version=1.2.0

    # List available versions
    needle upgrade --list

    # Air-gapped install
    needle upgrade --local=/path/to/needle-1.3.0

NOTES:
    - Running workers continue with old version until restarted
    - Use 'needle rollback' to revert if issues occur
    - Previous version backed up to ~/.needle/cache/
"
}

# -----------------------------------------------------------------------------
# Version Fetching
# -----------------------------------------------------------------------------

# Get latest version from GitHub releases
_needle_get_latest_version() {
    local api_url="$NEEDLE_GITHUB_API/repos/$NEEDLE_GITHUB_REPO/releases/latest"

    _needle_debug "Fetching latest version from $api_url"

    local response
    if _needle_command_exists curl; then
        response=$(curl -sf "$api_url" 2>/dev/null)
    elif _needle_command_exists wget; then
        response=$(wget -qO- "$api_url" 2>/dev/null)
    else
        _needle_error "Neither curl nor wget is available"
        return 1
    fi

    if [[ -z "$response" ]]; then
        _needle_error "Failed to fetch release information from GitHub"
        return 1
    fi

    # Extract tag name (e.g., "v0.1.0") and strip 'v' prefix
    local version
    version=$(echo "$response" | grep -m1 '"tag_name"' | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')

    if [[ -z "$version" ]]; then
        _needle_error "Could not parse version from GitHub response"
        return 1
    fi

    # Strip 'v' prefix if present
    echo "${version#v}"
}

# List available versions from GitHub
_needle_upgrade_list_versions() {
    local api_url="$NEEDLE_GITHUB_API/repos/$NEEDLE_GITHUB_REPO/releases"

    _needle_info "Fetching available versions..."

    local response
    if _needle_command_exists curl; then
        response=$(curl -sf "$api_url" 2>/dev/null)
    elif _needle_command_exists wget; then
        response=$(wget -qO- "$api_url" 2>/dev/null)
    else
        _needle_error "Neither curl nor wget is available"
        return 1
    fi

    if [[ -z "$response" ]]; then
        _needle_error "Failed to fetch releases from GitHub"
        return 1
    fi

    # Parse and display versions
    _needle_print ""
    _needle_print "Available versions:"
    _needle_print "────────────────────"

    echo "$response" | grep '"tag_name"' | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/' | while read -r tag; do
        local ver="${tag#v}"
        if [[ "$ver" == "$NEEDLE_VERSION" ]]; then
            _needle_print_color "$NEEDLE_COLOR_GREEN" "  $ver (current)"
        else
            _needle_print "  $ver"
        fi
    done

    _needle_print ""
}

# -----------------------------------------------------------------------------
# Download Functions
# -----------------------------------------------------------------------------

# Get download URL for a specific version
_needle_get_download_url() {
    local version="$1"
    local arch os

    # Detect architecture
    case "$(uname -m)" in
        x86_64|amd64)
            arch="amd64"
            ;;
        aarch64|arm64)
            arch="arm64"
            ;;
        armv7l|armhf)
            arch="arm"
            ;;
        *)
            _needle_error "Unsupported architecture: $(uname -m)"
            return 1
            ;;
    esac

    # Detect OS
    case "$(uname -s)" in
        Linux)
            os="linux"
            ;;
        Darwin)
            os="darwin"
            ;;
        *)
            _needle_error "Unsupported OS: $(uname -s)"
            return 1
            ;;
    esac

    # Construct download URL
    local filename="needle-${version}-${os}-${arch}"
    echo "https://github.com/$NEEDLE_GITHUB_REPO/releases/download/v${version}/${filename}"
}

# Get checksum URL for a specific version
_needle_get_checksum_url() {
    local version="$1"
    echo "https://github.com/$NEEDLE_GITHUB_REPO/releases/download/v${version}/checksums.sha256"
}

# Download file with progress
_needle_download_file() {
    local url="$1"
    local output="$2"

    _needle_verbose "Downloading: $url"
    _needle_verbose "Output: $output"

    if _needle_command_exists curl; then
        if ! curl -fL --progress-bar -o "$output" "$url" 2>&1; then
            _needle_error "Download failed: $url"
            return 1
        fi
    elif _needle_command_exists wget; then
        if ! wget -q --show-progress -O "$output" "$url" 2>&1; then
            _needle_error "Download failed: $url"
            return 1
        fi
    else
        _needle_error "Neither curl nor wget is available"
        return 1
    fi

    if [[ ! -f "$output" ]]; then
        _needle_error "Downloaded file not found: $output"
        return 1
    fi

    return 0
}

# Verify checksum
_needle_verify_checksum() {
    local file="$1"
    local version="$2"
    local checksum_file="$NEEDLE_DOWNLOAD_DIR/checksums.sha256"

    _needle_info "Verifying checksum..."

    # Download checksum file
    local checksum_url
    checksum_url=$(_needle_get_checksum_url "$version")
    if ! _needle_download_file "$checksum_url" "$checksum_file" 2>/dev/null; then
        _needle_warn "Could not download checksum file, skipping verification"
        return 0
    fi

    # Get expected checksum for our file
    local filename
    filename=$(basename "$file")
    local expected_checksum
    expected_checksum=$(grep "$filename" "$checksum_file" | awk '{print $1}')

    if [[ -z "$expected_checksum" ]]; then
        _needle_warn "Checksum not found for $filename, skipping verification"
        return 0
    fi

    # Calculate actual checksum
    local actual_checksum
    if _needle_command_exists sha256sum; then
        actual_checksum=$(sha256sum "$file" | awk '{print $1}')
    elif _needle_command_exists shasum; then
        actual_checksum=$(shasum -a 256 "$file" | awk '{print $1}')
    else
        _needle_warn "No checksum tool available, skipping verification"
        return 0
    fi

    if [[ "$actual_checksum" != "$expected_checksum" ]]; then
        _needle_error "Checksum mismatch!"
        _needle_error "Expected: $expected_checksum"
        _needle_error "Actual:   $actual_checksum"
        return 1
    fi

    _needle_success "Checksum verified"
    return 0
}

# -----------------------------------------------------------------------------
# Backup and Swap Functions
# -----------------------------------------------------------------------------

# Get current binary path
_needle_get_binary_path() {
    # Resolve the actual needle binary path
    local needle_path

    if [[ -n "$NEEDLE_SCRIPT_DIR" && -f "$NEEDLE_SCRIPT_DIR/needle" ]]; then
        needle_path="$NEEDLE_SCRIPT_DIR/needle"
    elif _needle_command_exists needle; then
        needle_path=$(command -v needle)
    else
        _needle_error "Cannot locate needle binary"
        return 1
    fi

    # Resolve symlinks
    while [[ -L "$needle_path" ]]; do
        needle_path=$(readlink "$needle_path")
    done

    echo "$needle_path"
}

# Create backup of current binary
_needle_create_backup() {
    local current_binary="$1"
    local version="$2"
    local backup_file="$NEEDLE_BACKUP_DIR/needle-${version}-$(date +%Y%m%d%H%M%S)"

    _needle_ensure_upgrade_dirs

    if ! cp "$current_binary" "$backup_file"; then
        _needle_error "Failed to create backup"
        return 1
    fi

    chmod +x "$backup_file"
    _needle_success "Backup created: $backup_file"

    # Clean old backups
    _needle_clean_old_backups

    echo "$backup_file"
}

# Clean old backups (keep only NEEDLE_MAX_BACKUPS)
_needle_clean_old_backups() {
    local backup_count
    backup_count=$(find "$NEEDLE_BACKUP_DIR" -name "needle-*" -type f 2>/dev/null | wc -l)

    if [[ $backup_count -gt $NEEDLE_MAX_BACKUPS ]]; then
        local to_delete=$((backup_count - NEEDLE_MAX_BACKUPS))
        _needle_verbose "Cleaning $to_delete old backup(s)"

        find "$NEEDLE_BACKUP_DIR" -name "needle-*" -type f -printf '%T@ %p\n' 2>/dev/null | \
            sort -n | head -n "$to_delete" | cut -d' ' -f2- | \
            while read -r file; do
                rm -f "$file"
                _needle_verbose "Removed old backup: $file"
            done
    fi
}

# List available backups
_needle_list_backups() {
    if [[ ! -d "$NEEDLE_BACKUP_DIR" ]]; then
        _needle_info "No backups available"
        return 0
    fi

    local backups
    backups=$(find "$NEEDLE_BACKUP_DIR" -name "needle-*" -type f 2>/dev/null | sort -r)

    if [[ -z "$backups" ]]; then
        _needle_info "No backups available"
        return 0
    fi

    _needle_print ""
    _needle_print "Available backups:"
    _needle_print "───────────────────"

    while read -r backup; do
        local filename
        filename=$(basename "$backup")
        # Extract version from filename (needle-VERSION-TIMESTAMP)
        local version
        version=$(echo "$filename" | sed 's/needle-\([^-]*\)-.*/\1/')
        _needle_print "  $version  ($filename)"
    done <<< "$backups"

    _needle_print ""
}

# Atomic binary swap
_needle_perform_swap() {
    local new_binary="$1"
    local target_path="$2"
    local target_dir
    target_dir=$(dirname "$target_path")

    _needle_info "Performing atomic swap..."

    # Ensure target directory exists and is writable
    if [[ ! -d "$target_dir" ]]; then
        _needle_error "Target directory does not exist: $target_dir"
        return 1
    fi

    if [[ ! -w "$target_dir" ]]; then
        _needle_error "Target directory is not writable: $target_dir"
        _needle_info "You may need to run with elevated permissions"
        return 1
    fi

    # Use atomic rename with temp file for safety
    local temp_path="${target_path}.new.$$"
    local old_path="${target_path}.old.$$"

    # Copy new binary to temp location
    if ! cp "$new_binary" "$temp_path"; then
        _needle_error "Failed to copy new binary"
        rm -f "$temp_path"
        return 1
    fi

    chmod +x "$temp_path"

    # Atomic rename sequence
    if ! mv "$target_path" "$old_path" 2>/dev/null; then
        # Target might not exist yet, that's ok
        rm -f "$old_path"
    fi

    if ! mv "$temp_path" "$target_path"; then
        # Rollback
        _needle_error "Failed to install new binary"
        if [[ -f "$old_path" ]]; then
            mv "$old_path" "$target_path"
            _needle_info "Rolled back to previous binary"
        fi
        rm -f "$temp_path"
        return 1
    fi

    # Clean up old file
    rm -f "$old_path"

    _needle_success "Binary installed successfully"
    return 0
}

# -----------------------------------------------------------------------------
# Rollback Functions
# -----------------------------------------------------------------------------

# Rollback to previous version
_needle_upgrade_rollback() {
    local target_version="${1:-}"

    if [[ ! -d "$NEEDLE_BACKUP_DIR" ]]; then
        _needle_error "No backups available for rollback"
        return 1
    fi

    local backup_file

    if [[ -n "$target_version" ]]; then
        # Find specific version backup
        backup_file=$(find "$NEEDLE_BACKUP_DIR" -name "needle-${target_version}-*" -type f 2>/dev/null | head -1)
        if [[ -z "$backup_file" ]]; then
            _needle_error "No backup found for version $target_version"
            _needle_list_backups
            return 1
        fi
    else
        # Find most recent backup
        backup_file=$(find "$NEEDLE_BACKUP_DIR" -name "needle-*" -type f -printf '%T@ %p\n' 2>/dev/null | sort -rn | head -1 | cut -d' ' -f2-)
        if [[ -z "$backup_file" ]]; then
            _needle_error "No backups available for rollback"
            return 1
        fi
    fi

    local backup_version
    backup_version=$(basename "$backup_file" | sed 's/needle-\([^-]*\)-.*/\1/')

    _needle_info "Rolling back to version $backup_version..."
    _needle_verbose "Backup file: $backup_file"

    # Get current binary path
    local current_binary
    current_binary=$(_needle_get_binary_path)
    if [[ -z "$current_binary" ]]; then
        return 1
    fi

    # Create backup of current version before rollback
    if ! _needle_create_backup "$current_binary" "$NEEDLE_VERSION"; then
        _needle_warn "Could not create backup before rollback"
    fi

    # Perform swap
    if _needle_perform_swap "$backup_file" "$current_binary"; then
        _needle_success "Rolled back to version $backup_version"
        _needle_info "Run 'needle version' to verify"
        return 0
    else
        return 1
    fi
}

# -----------------------------------------------------------------------------
# Local Install Functions
# -----------------------------------------------------------------------------

# Install from local file
_needle_upgrade_from_local() {
    local local_file="$1"

    if [[ ! -f "$local_file" ]]; then
        _needle_error "File not found: $local_file"
        return 1
    fi

    if [[ ! -x "$local_file" ]]; then
        _needle_warn "File is not executable, attempting to set permissions"
        chmod +x "$local_file"
    fi

    _needle_info "Installing from local file: $local_file"

    # Get current binary path
    local current_binary
    current_binary=$(_needle_get_binary_path)
    if [[ -z "$current_binary" ]]; then
        return 1
    fi

    # Create backup
    if ! _needle_create_backup "$current_binary" "$NEEDLE_VERSION"; then
        _needle_warn "Could not create backup"
    fi

    # Perform swap
    if _needle_perform_swap "$local_file" "$current_binary"; then
        _needle_success "Local installation complete"
        return 0
    else
        return 1
    fi
}

# -----------------------------------------------------------------------------
# Directory Management
# -----------------------------------------------------------------------------

_needle_ensure_upgrade_dirs() {
    mkdir -p "$NEEDLE_UPGRADE_DIR"
    mkdir -p "$NEEDLE_BACKUP_DIR"
    mkdir -p "$NEEDLE_DOWNLOAD_DIR"
}

# -----------------------------------------------------------------------------
# Check for Updates
# -----------------------------------------------------------------------------

_needle_upgrade_check() {
    _needle_info "Checking for updates..."

    local latest
    latest=$(_needle_get_latest_version)
    if [[ -z "$latest" ]]; then
        _needle_error "Failed to check for updates"
        return 1
    fi

    local current="$NEEDLE_VERSION"

    _needle_print ""
    _needle_print "Current version: $current"
    _needle_print "Latest version:  $latest"
    _needle_print ""

    if [[ "$latest" == "$current" ]]; then
        _needle_success "Already at latest version"
        return 0
    fi

    # Compare versions
    if _needle_version_compare "$latest" "$current"; then
        _needle_info "Update available: $current -> $latest"
        _needle_print ""
        _needle_print "Run 'needle upgrade' to install the update"
        return 0
    else
        _needle_warn "Current version is newer than latest release"
        return 0
    fi
}

# -----------------------------------------------------------------------------
# Main Upgrade Function
# -----------------------------------------------------------------------------

_needle_upgrade() {
    local check_only=false
    local force=false
    local yes=false
    local version="latest"
    local local_file=""
    local list=false
    local rollback=false
    local rollback_to=""

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -c|--check)
                check_only=true
                shift
                ;;
            -f|--force)
                force=true
                shift
                ;;
            -y|--yes)
                yes=true
                shift
                ;;
            -l|--local)
                if [[ -z "${2:-}" ]]; then
                    _needle_error "Option --local requires a file path"
                    exit $NEEDLE_EXIT_USAGE
                fi
                local_file="$2"
                shift 2
                ;;
            -v|--version)
                if [[ -z "${2:-}" ]]; then
                    _needle_error "Option --version requires a version number"
                    exit $NEEDLE_EXIT_USAGE
                fi
                version="$2"
                shift 2
                ;;
            --list)
                list=true
                shift
                ;;
            --rollback)
                rollback=true
                shift
                ;;
            --rollback-to)
                if [[ -z "${2:-}" ]]; then
                    _needle_error "Option --rollback-to requires a version number"
                    exit $NEEDLE_EXIT_USAGE
                fi
                rollback=true
                rollback_to="$2"
                shift 2
                ;;
            -h|--help)
                _needle_upgrade_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                _needle_error "Unknown option: $1"
                _needle_upgrade_help
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    # Ensure directories exist
    _needle_ensure_upgrade_dirs

    # Handle list versions
    if $list; then
        _needle_upgrade_list_versions
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # Handle rollback
    if $rollback; then
        _needle_upgrade_rollback "$rollback_to"
        exit $?
    fi

    # Handle check only
    if $check_only; then
        _needle_upgrade_check
        exit $?
    fi

    # Handle local install
    if [[ -n "$local_file" ]]; then
        _needle_upgrade_from_local "$local_file"
        exit $?
    fi

    # Get latest version
    _needle_info "Checking for updates..."

    local latest
    latest=$(_needle_get_latest_version)
    if [[ -z "$latest" ]]; then
        _needle_error "Failed to fetch latest version"
        exit $NEEDLE_EXIT_ERROR
    fi

    local target_version="$latest"
    if [[ "$version" != "latest" ]]; then
        target_version="$version"
    fi

    local current="$NEEDLE_VERSION"

    # Check if update needed
    if [[ "$target_version" == "$current" ]] && ! $force; then
        _needle_success "Already at version $current"
        _needle_info "Use --force to reinstall"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    _needle_print ""
    _needle_print "Upgrade Summary:"
    _needle_print "────────────────"
    _needle_print "  Current:  $current"
    _needle_print "  Target:   $target_version"
    _needle_print ""

    # Confirm upgrade
    if ! $yes; then
        if ! _needle_confirm "Proceed with upgrade?" "n"; then
            _needle_info "Upgrade cancelled"
            exit $NEEDLE_EXIT_SUCCESS
        fi
    fi

    # Get download URL
    local download_url
    download_url=$(_needle_get_download_url "$target_version")
    if [[ -z "$download_url" ]]; then
        _needle_error "Could not determine download URL"
        exit $NEEDLE_EXIT_ERROR
    fi

    # Download new binary
    local download_file="$NEEDLE_DOWNLOAD_DIR/needle-${target_version}"
    _needle_info "Downloading version $target_version..."

    if ! _needle_download_file "$download_url" "$download_file"; then
        exit $NEEDLE_EXIT_ERROR
    fi

    # Verify checksum
    if ! _needle_verify_checksum "$download_file" "$target_version"; then
        _needle_error "Checksum verification failed"
        rm -f "$download_file"
        exit $NEEDLE_EXIT_ERROR
    fi

    # Make executable
    chmod +x "$download_file"

    # Get current binary path
    local current_binary
    current_binary=$(_needle_get_binary_path)
    if [[ -z "$current_binary" ]]; then
        exit $NEEDLE_EXIT_ERROR
    fi

    # Create backup
    _needle_info "Creating backup..."
    local backup_file
    backup_file=$(_needle_create_backup "$current_binary" "$current")
    if [[ -z "$backup_file" ]]; then
        _needle_warn "Backup creation failed, proceeding anyway"
    fi

    # Perform atomic swap
    if _needle_perform_swap "$download_file" "$current_binary"; then
        _needle_print ""
        _needle_success "Upgraded to version $target_version!"
        _needle_info "Run 'needle version' to verify"
        _needle_info "Run 'needle upgrade --rollback' to revert if needed"

        # Clean up download
        rm -f "$download_file"

        exit $NEEDLE_EXIT_SUCCESS
    else
        _needle_error "Upgrade failed"

        # Attempt rollback
        if [[ -n "$backup_file" ]] && [[ -f "$backup_file" ]]; then
            _needle_info "Attempting rollback..."
            if _needle_perform_swap "$backup_file" "$current_binary"; then
                _needle_success "Rolled back to version $current"
            fi
        fi

        exit $NEEDLE_EXIT_ERROR
    fi
}
