#!/usr/bin/env bash
# NEEDLE CLI Setup Subcommand
# Check and install NEEDLE dependencies

# -----------------------------------------------------------------------------
# Help
# -----------------------------------------------------------------------------

_needle_setup_help() {
    _needle_print "Check and install NEEDLE dependencies

This command checks for required dependencies (tmux, jq, yq, br) and
optionally installs any that are missing.

USAGE:
    needle setup [OPTIONS]

OPTIONS:
    -c, --check       Check only, don't install anything
    -r, --reinstall   Force reinstall all dependencies
    -y, --yes         Don't prompt for confirmation
    -j, --json        Output in JSON format
    -h, --help        Show this help message

EXAMPLES:
    # Check and install missing dependencies
    needle setup

    # Check only without installing
    needle setup --check

    # Reinstall all dependencies
    needle setup --reinstall

    # Install without prompting
    needle setup --yes

    # Get dependency status as JSON
    needle setup --json
"
}

# -----------------------------------------------------------------------------
# JSON Output
# -----------------------------------------------------------------------------

# Output dependency status as JSON
_needle_setup_json() {
    # Run dependency check silently
    _needle_check_deps &>/dev/null || true

    local first=true

    echo "{"

    for dep in "${!NEEDLE_DEPS[@]}"; do
        local status="ok"
        local version="0.0"
        local required="${NEEDLE_DEPS[$dep]}"
        local installed=false

        if _dep_is_installed "$dep"; then
            installed=true
            version=$(_parse_dep_version "$dep")

            if ! _version_gte "$version" "$required"; then
                status="outdated"
            fi
        else
            status="missing"
        fi

        if [[ "$first" == "true" ]]; then
            first=false
        else
            echo ","
        fi

        printf '  "%s": {"status": "%s", "version": "%s", "required": "%s", "installed": %s}' \
            "$dep" "$status" "$version" "$required" "$installed"
    done

    echo ""
    echo "}"
}

# -----------------------------------------------------------------------------
# Main Setup Command
# -----------------------------------------------------------------------------

_needle_setup() {
    local check_only=false
    local reinstall=false
    local auto_yes=false
    local json_output=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -c|--check)
                check_only=true
                shift
                ;;
            -r|--reinstall)
                reinstall=true
                shift
                ;;
            -y|--yes)
                auto_yes=true
                shift
                ;;
            -j|--json)
                json_output=true
                shift
                ;;
            -h|--help)
                _needle_setup_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                _needle_error "Unknown option: $1"
                _needle_print ""
                _needle_setup_help
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    # JSON output mode
    if [[ "$json_output" == "true" ]]; then
        _needle_setup_json
        exit $NEEDLE_EXIT_SUCCESS
    fi

    _needle_info "Checking dependencies..."
    _needle_print ""

    # Arrays to track status
    local missing=()
    local outdated=()
    local installed=()

    # Check each dependency
    for dep in "${!NEEDLE_DEPS[@]}"; do
        local required="${NEEDLE_DEPS[$dep]}"
        local name="${NEEDLE_DEPS_NAMES[$dep]:-$dep}"

        if _dep_is_installed "$dep" && [[ "$reinstall" != "true" ]]; then
            local version
            version=$(_parse_dep_version "$dep")

            if _version_gte "$version" "$required"; then
                _needle_success "$dep $version"
                installed+=("$dep")
            else
                _needle_warn "$dep $version (need $required)"
                outdated+=("$dep")
            fi
        else
            if [[ "$reinstall" == "true" ]] && _dep_is_installed "$dep"; then
                local version
                version=$(_parse_dep_version "$dep")
                _needle_warn "$dep $version (forcing reinstall)"
            else
                _needle_warn "$dep not found"
            fi
            missing+=("$dep")
        fi
    done

    _needle_print ""

    # If all dependencies are satisfied
    if [[ ${#missing[@]} -eq 0 && ${#outdated[@]} -eq 0 ]]; then
        _needle_success "All dependencies installed!"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # If check-only mode, exit with error
    if [[ "$check_only" == "true" ]]; then
        if [[ ${#missing[@]} -gt 0 ]]; then
            _needle_error "Missing: ${missing[*]}"
        fi
        if [[ ${#outdated[@]} -gt 0 ]]; then
            _needle_error "Outdated: ${outdated[*]}"
        fi
        exit $NEEDLE_EXIT_DEPENDENCY
    fi

    # Combine missing and outdated for installation
    local to_install=("${missing[@]}")
    if [[ "$reinstall" == "true" ]]; then
        to_install=("${missing[@]}" "${outdated[@]}")
    fi

    # If nothing to install
    if [[ ${#to_install[@]} -eq 0 ]]; then
        _needle_info "All dependencies are up to date."
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # Prompt for confirmation
    if [[ "$auto_yes" != "true" ]]; then
        _needle_print "Missing dependencies: ${to_install[*]}"
        _needle_print ""
        read -p "Install missing dependencies? [Y/n] " -n 1 -r
        _needle_print ""
        if [[ $REPLY =~ ^[Nn]$ ]]; then
            _needle_warn "Installation cancelled."
            exit $NEEDLE_EXIT_CANCELLED
        fi
    fi

    _needle_print ""

    # Setup PATH for binary downloads
    _needle_setup_path

    # Install each missing dependency
    local failed=()
    local succeeded=()

    for dep in "${to_install[@]}"; do
        _needle_info "Installing $dep..."

        # Get the installer function
        local installer="_needle_install_$dep"

        if declare -f "$installer" &>/dev/null; then
            if "$installer"; then
                # Verify installation
                if _dep_is_installed "$dep"; then
                    local version
                    version=$(_parse_dep_version "$dep")
                    _needle_success "$dep $version installed"
                    succeeded+=("$dep")
                else
                    _needle_error "Failed to install $dep (not found after install)"
                    failed+=("$dep")
                fi
            else
                _needle_error "Failed to install $dep"
                failed+=("$dep")
            fi
        else
            _needle_error "No installer available for $dep"
            failed+=("$dep")
        fi
    done

    _needle_print ""

    # Update PATH persistence if we installed anything
    if [[ ${#succeeded[@]} -gt 0 ]]; then
        local cache_dir
        cache_dir=$(_needle_get_cache_dir)
        _needle_add_to_shell_rc "$cache_dir" || true
    fi

    # Summary
    if [[ ${#failed[@]} -eq 0 ]]; then
        _needle_success "Setup complete!"
        exit $NEEDLE_EXIT_SUCCESS
    else
        _needle_error "Failed to install: ${failed[*]}"
        exit $NEEDLE_EXIT_DEPENDENCY
    fi
}
