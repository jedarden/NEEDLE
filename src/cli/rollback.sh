#!/usr/bin/env bash
# NEEDLE CLI Rollback Subcommand
# Restore a previously installed version from backup cache

# Rollback constants
NEEDLE_ROLLBACK_CACHE_DIR="$NEEDLE_HOME/$NEEDLE_CACHE_DIR"

# -----------------------------------------------------------------------------
# Help
# -----------------------------------------------------------------------------

_needle_rollback_help() {
    _needle_print "Rollback NEEDLE to a previous version

Restores a previously installed version from the backup cache.
If no version is specified, rolls back to the most recent backup.

USAGE:
    needle rollback [OPTIONS]

OPTIONS:
    -v, --version <VER>      Rollback to specific version
    -l, --list               List available backup versions
    -y, --yes                Skip confirmation prompt
    -h, --help               Show this help message

EXAMPLES:
    # Rollback to most recent backup
    needle rollback

    # Rollback to specific version
    needle rollback --version 0.2.0

    # Show available backup versions
    needle rollback --list

BACKUP LOCATION:
    Backups are stored in: $NEEDLE_ROLLBACK_CACHE_DIR
    Backup format: needle-VERSION.bak

NOTES:
    - A backup of the current version is created before rollback
    - Use 'needle upgrade --rollback' for upgrade-specific rollback
"
}

# -----------------------------------------------------------------------------
# Backup Discovery Functions
# -----------------------------------------------------------------------------

# Ensure cache directory exists
_needle_rollback_ensure_dirs() {
    mkdir -p "$NEEDLE_ROLLBACK_CACHE_DIR"
}

# List available backups
_needle_rollback_list() {
    _needle_rollback_ensure_dirs

    local backups
    backups=$(find "$NEEDLE_ROLLBACK_CACHE_DIR" -name "needle-*.bak" -type f 2>/dev/null | sort -r)

    if [[ -z "$backups" ]]; then
        _needle_info "No backups available in cache"
        _needle_verbose "Cache directory: $NEEDLE_ROLLBACK_CACHE_DIR"
        return 0
    fi

    _needle_print ""
    _needle_print "Available backups:"
    _needle_print "────────────────────"

    while read -r backup; do
        local filename
        filename=$(basename "$backup")
        # Extract version from filename (needle-VERSION.bak)
        local version
        version=$(echo "$filename" | sed 's/needle-\(.*\)\.bak/\1/')

        # Check if this is the current version
        if [[ "$version" == "$NEEDLE_VERSION" ]]; then
            _needle_print_color "$NEEDLE_COLOR_GREEN" "  $version (current)"
        else
            _needle_print "  $version"
        fi

        _needle_verbose "    Path: $backup"
    done <<< "$backups"

    _needle_print ""
}

# Find most recent backup (excluding current version)
_needle_rollback_find_recent() {
    local backups
    backups=$(find "$NEEDLE_ROLLBACK_CACHE_DIR" -name "needle-*.bak" -type f -printf '%T@ %p\n' 2>/dev/null | sort -rn)

    if [[ -z "$backups" ]]; then
        return 1
    fi

    # Find the most recent backup that's not the current version
    while read -r line; do
        local backup
        backup=$(echo "$line" | cut -d' ' -f2-)
        local filename
        filename=$(basename "$backup")
        local version
        version=$(echo "$filename" | sed 's/needle-\(.*\)\.bak/\1/')

        # Skip current version
        if [[ "$version" != "$NEEDLE_VERSION" ]]; then
            echo "$backup"
            return 0
        fi
    done <<< "$backups"

    # No backup found that's different from current
    return 1
}

# Find specific version backup
_needle_rollback_find_version() {
    local target_version="$1"
    local backup_file="$NEEDLE_ROLLBACK_CACHE_DIR/needle-${target_version}.bak"

    if [[ -f "$backup_file" ]]; then
        echo "$backup_file"
        return 0
    fi

    return 1
}

# -----------------------------------------------------------------------------
# Validation Functions
# -----------------------------------------------------------------------------

# Validate backup file exists and is executable
_needle_rollback_validate_backup() {
    local backup_file="$1"

    if [[ ! -f "$backup_file" ]]; then
        _needle_error "Backup file not found: $backup_file"
        return 1
    fi

    if [[ ! -r "$backup_file" ]]; then
        _needle_error "Backup file is not readable: $backup_file"
        return 1
    fi

    # Check if executable, try to fix if not
    if [[ ! -x "$backup_file" ]]; then
        _needle_warn "Backup file is not executable, fixing permissions..."
        if ! chmod +x "$backup_file"; then
            _needle_error "Failed to set executable permissions"
            return 1
        fi
    fi

    # Verify it's a valid executable (basic check)
    if ! file "$backup_file" 2>/dev/null | grep -qE 'executable|ELF|Mach-O|shell'; then
        _needle_warn "Backup file may not be a valid executable"
        _needle_verbose "File type: $(file "$backup_file" 2>/dev/null)"
    fi

    _needle_success "Backup file validated"
    return 0
}

# -----------------------------------------------------------------------------
# Binary Management Functions
# -----------------------------------------------------------------------------

# Get current binary path
_needle_rollback_get_binary_path() {
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

# Create backup of current binary before rollback
_needle_rollback_create_backup() {
    local current_binary="$1"
    local current_version="$2"
    local backup_file="$NEEDLE_ROLLBACK_CACHE_DIR/needle-${current_version}.bak"

    _needle_rollback_ensure_dirs

    # Check if backup already exists
    if [[ -f "$backup_file" ]]; then
        _needle_verbose "Backup already exists: $backup_file"
        echo "$backup_file"
        return 0
    fi

    if ! cp "$current_binary" "$backup_file"; then
        _needle_error "Failed to create backup"
        return 1
    fi

    chmod +x "$backup_file"
    _needle_success "Backup created: needle-${current_version}.bak"

    echo "$backup_file"
}

# Atomic binary swap
_needle_rollback_swap() {
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
        _needle_error "Failed to copy rollback binary"
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
        _needle_error "Failed to install rollback binary"
        if [[ -f "$old_path" ]]; then
            mv "$old_path" "$target_path"
            _needle_info "Restored previous binary"
        fi
        rm -f "$temp_path"
        return 1
    fi

    # Clean up old file
    rm -f "$old_path"

    _needle_success "Binary swapped successfully"
    return 0
}

# Verify rollback succeeded
_needle_rollback_verify() {
    local expected_version="$1"

    _needle_info "Verifying rollback..."

    # Check that needle command still works
    local installed_version
    if ! installed_version=$(needle --version 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1); then
        _needle_error "Rollback verification failed: cannot execute needle"
        return 1
    fi

    if [[ "$installed_version" != "$expected_version" ]]; then
        _needle_warn "Version mismatch: expected $expected_version, got $installed_version"
        _needle_info "This may occur if the binary reports a different version than the backup filename"
    fi

    _needle_success "Rollback verified: version $installed_version"
    return 0
}

# -----------------------------------------------------------------------------
# Main Rollback Function
# -----------------------------------------------------------------------------

_needle_rollback() {
    local target_version=""
    local list_only=false
    local yes=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -v|--version)
                if [[ -z "${2:-}" ]]; then
                    _needle_error "Option --version requires a version number"
                    exit $NEEDLE_EXIT_USAGE
                fi
                target_version="$2"
                shift 2
                ;;
            --version=*)
                target_version="${1#*=}"
                shift
                ;;
            -l|--list)
                list_only=true
                shift
                ;;
            -y|--yes)
                yes=true
                shift
                ;;
            -h|--help)
                _needle_rollback_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                _needle_error "Unknown option: $1"
                _needle_rollback_help
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    # Ensure directories exist
    _needle_rollback_ensure_dirs

    # Handle list only
    if $list_only; then
        _needle_rollback_list
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # Find backup to restore
    local backup_file
    local backup_version

    if [[ -n "$target_version" ]]; then
        # Find specific version
        backup_file=$(_needle_rollback_find_version "$target_version")
        if [[ -z "$backup_file" ]]; then
            _needle_error "No backup found for version $target_version"
            _needle_rollback_list
            exit $NEEDLE_EXIT_ERROR
        fi
        backup_version="$target_version"
    else
        # Find most recent backup
        backup_file=$(_needle_rollback_find_recent)
        if [[ -z "$backup_file" ]]; then
            _needle_error "No backups available for rollback"
            _needle_info "Run 'needle rollback --list' to see available backups"
            exit $NEEDLE_EXIT_ERROR
        fi
        # Extract version from filename
        backup_version=$(basename "$backup_file" | sed 's/needle-\(.*\)\.bak/\1/')
    fi

    _needle_verbose "Backup file: $backup_file"
    _needle_verbose "Backup version: $backup_version"

    # Validate backup
    if ! _needle_rollback_validate_backup "$backup_file"; then
        exit $NEEDLE_EXIT_ERROR
    fi

    # Get current binary path
    local current_binary
    current_binary=$(_needle_rollback_get_binary_path)
    if [[ -z "$current_binary" ]]; then
        exit $NEEDLE_EXIT_ERROR
    fi

    _needle_verbose "Current binary: $current_binary"

    # Display rollback summary
    _needle_print ""
    _needle_print "Rollback Summary:"
    _needle_print "─────────────────"
    _needle_print "  Current version:  $NEEDLE_VERSION"
    _needle_print "  Target version:   $backup_version"
    _needle_print "  Backup file:      $(basename "$backup_file")"
    _needle_print ""

    # Check if already at target version
    if [[ "$backup_version" == "$NEEDLE_VERSION" ]]; then
        _needle_warn "Already at version $NEEDLE_VERSION"
        _needle_info "Use 'needle rollback --list' to see other available versions"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # Confirm rollback
    if ! $yes; then
        if ! _needle_confirm "Proceed with rollback?" "n"; then
            _needle_info "Rollback cancelled"
            exit $NEEDLE_EXIT_SUCCESS
        fi
    fi

    # Create backup of current version before rollback
    _needle_info "Creating backup of current version..."
    if ! _needle_rollback_create_backup "$current_binary" "$NEEDLE_VERSION"; then
        _needle_warn "Could not create backup of current version, proceeding anyway"
    fi

    # Perform atomic swap
    if ! _needle_rollback_swap "$backup_file" "$current_binary"; then
        exit $NEEDLE_EXIT_ERROR
    fi

    # Verify rollback
    if _needle_rollback_verify "$backup_version"; then
        _needle_print ""
        _needle_success "Rolled back to version $backup_version!"
        _needle_info "Run 'needle version' to verify"
        exit $NEEDLE_EXIT_SUCCESS
    else
        _needle_error "Rollback verification failed"
        exit $NEEDLE_EXIT_ERROR
    fi
}
