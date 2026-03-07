#!/usr/bin/env bash
# NEEDLE CLI Init Subcommand
# Initialize NEEDLE configuration and directory structure

# -----------------------------------------------------------------------------
# Dependency Check for Init
# -----------------------------------------------------------------------------

# Check dependencies and display in init format
# Output format:
#   ✓ br       0.8.0    installed
#   ✗ tmux     -        NOT FOUND
#
# Returns: 0 if all deps present, 1 if missing
_needle_init_check_deps() {
    local check_only="${1:-false}"

    echo "Checking dependencies..."

    # Run dependency check silently
    _needle_check_deps &>/dev/null || true

    local all_ok=true
    local missing_list=()

    # Process each dependency in a consistent order
    local deps_order=("br" "yq" "jq" "tmux")

    for dep in "${deps_order[@]}"; do
        local status
        status=$(_check_single_dep "$dep")
        local version
        version=$(_parse_dep_version "$dep")

        case "$status" in
            ok)
                printf "  \033[0;32m✓\033[0m %-8s %-8s %s\n" "$dep" "$version" "installed"
                ;;
            missing)
                printf "  \033[0;31m✗\033[0m %-8s %-8s %s\n" "$dep" "-" "NOT FOUND"
                missing_list+=("$dep")
                all_ok=false
                ;;
            outdated:*)
                local have_version="${status#outdated:}"
                local need_version="${NEEDLE_DEPS[$dep]}"
                printf "  \033[0;33m⚠\033[0m %-8s %-8s %s\n" "$dep" "$have_version" "outdated (need $need_version)"
                # Treat outdated as OK for init purposes (still functional)
                ;;
        esac
    done

    # If checking only, just return the status
    if [[ "$check_only" == "true" ]]; then
        $all_ok
        return $?
    fi

    # Show missing deps message and exit if any missing
    if [[ ${#missing_list[@]} -gt 0 ]]; then
        echo ""
        echo "Missing dependencies: ${missing_list[*]}"
        echo "Run 'needle setup' to install missing dependencies"
        return 1
    fi

    echo ""
    return 0
}

_needle_init_help() {
    _needle_print "Initialize NEEDLE with interactive onboarding

This command guides you through first-time setup:
  1. Installing dependencies (tmux, jq, yq, br)
  2. Detecting available coding CLI agents
  3. Configuring your first workspace
  4. Creating default configuration

USAGE:
    needle init [OPTIONS]

OPTIONS:
    -f, --force              Overwrite existing configuration
    -n, --non-interactive    Run without prompts (use defaults)

    --workspace <PATH>       Preset workspace path (skips prompt)
    --agent <NAME>           Preset agent name (skips prompt)

    -h, --help               Print help information

EXAMPLES:
    # Interactive setup
    needle init

    # Re-run setup, overwriting existing config
    needle init --force

    # Scripted setup for CI/CD
    needle init --non-interactive --workspace=/app --agent=claude-anthropic-sonnet

    # Check if setup is needed
    if needle init --check; then
        needle init --non-interactive
    fi

NOTES:
    - Any needle command in an unconfigured environment auto-redirects here
    - Re-running init without --force preserves existing configuration
    - Use --force to completely reset and reconfigure
"
}

_needle_init() {
    local force=false
    local use_defaults=false
    local workspace=""
    local editor="${EDITOR:-vim}"
    local timezone="UTC"

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -f|--force)
                force=true
                shift
                ;;
            -d|--defaults)
                use_defaults=true
                shift
                ;;
            -w|--workspace)
                workspace="$2"
                shift 2
                ;;
            -e|--editor)
                editor="$2"
                shift 2
                ;;
            -t|--timezone)
                timezone="$2"
                shift 2
                ;;
            -h|--help)
                _needle_init_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                _needle_error "Unknown option: $1"
                _needle_init_help
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    # Show welcome banner
    _needle_welcome_init

    # -------------------------------------------------------------------------
    # Step 0: Check Dependencies
    # -------------------------------------------------------------------------
    if ! _needle_init_check_deps; then
        # Missing dependencies - exit with error
        exit $NEEDLE_EXIT_CONFIG
    fi

    # Check if already initialized
    if _needle_is_initialized && [[ "$force" != "true" ]]; then
        _needle_warn "NEEDLE is already initialized at $NEEDLE_HOME"
        _needle_info "Use --force to reinitialize"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    _needle_header "Initializing NEEDLE"

    # -------------------------------------------------------------------------
    # Step 1: Create directory structure
    # -------------------------------------------------------------------------
    _needle_section "Directory Setup"

    # Create NEEDLE home directory
    if [[ ! -d "$NEEDLE_HOME" ]]; then
        _needle_verbose "Creating NEEDLE home: $NEEDLE_HOME"
        mkdir -p "$NEEDLE_HOME"
    fi

    # Create subdirectories using onboarding module (creates hooks dir too)
    _needle_create_config_dirs "$NEEDLE_HOME"

    # -------------------------------------------------------------------------
    # Step 2: Workspace setup
    # -------------------------------------------------------------------------
    _needle_section "Workspace Setup"

    local workspace_path="${workspace:-$(pwd)}"

    if [[ "$use_defaults" == "true" ]]; then
        # Non-interactive: use provided or current directory
        workspace_path=$(cd "$workspace_path" 2>/dev/null && pwd)

        # Validate workspace
        if ! _needle_is_valid_workspace "$workspace_path"; then
            _needle_warn "No .beads directory in workspace: $workspace_path"
            _needle_info "Initialize with: cd $workspace_path && br init"
            _needle_info "Continuing with NEEDLE configuration..."
        fi
    else
        # Interactive: prompt for workspace
        if ! _needle_workspace_setup --offer-init --default "$workspace_path"; then
            _needle_warn "Workspace setup incomplete, continuing with configuration"
        fi

        # Get the validated workspace (may have been set by workspace_setup)
        workspace_path="${NEEDLE_WORKSPACE:-$workspace_path}"
    fi

    # Store workspace for config generation
    export NEEDLE_CONFIG_WORKSPACE="$workspace_path"

    # -------------------------------------------------------------------------
    # Step 3: Create configuration with interactive prompts
    # -------------------------------------------------------------------------
    local config_args=()

    if [[ "$force" == "true" ]]; then
        config_args+=("--force")
    fi

    if [[ "$use_defaults" == "true" ]]; then
        config_args+=("--defaults")
    fi

    if ! _needle_onboarding_create_config "${config_args[@]}"; then
        _needle_error "Failed to create configuration"
        exit $NEEDLE_EXIT_CONFIG
    fi

    # Update config with editor and timezone
    # Note: NEEDLE_CONFIG_FILE is already the full path (set in constants.sh)
    local config_file="$NEEDLE_CONFIG_FILE"
    if [[ -f "$config_file" ]]; then
        _needle_config_set "editor" "\"$editor\""
        _needle_config_set "timezone" "\"$timezone\""

        # Add workspace to config if we have one
        if [[ -n "$NEEDLE_CONFIG_WORKSPACE" ]]; then
            # Check if workspaces section exists, if not create it
            if ! grep -q "^workspaces:" "$config_file" 2>/dev/null; then
                echo "" >> "$config_file"
                echo "workspaces:" >> "$config_file"
            fi
            # Add the workspace path
            echo "  - \"$NEEDLE_CONFIG_WORKSPACE\"" >> "$config_file"
        fi

        _needle_success "Updated configuration settings"
    fi

    # -------------------------------------------------------------------------
    # Step 4: Create README
    # -------------------------------------------------------------------------
    local readme="$NEEDLE_HOME/README.md"
    if [[ ! -f "$readme" ]] || [[ "$force" == "true" ]]; then
        cat > "$readme" << EOF
# NEEDLE Configuration

This directory contains your NEEDLE configuration and state.

## Structure

- \`config.yaml\` - Main configuration file
- \`state/\` - Runtime state files
- \`cache/\` - Cached data
- \`logs/\` - Log files
- \`hooks/\` - Hook scripts

## Configuration

Edit \`config.yaml\` to customize NEEDLE behavior.

## More Information

Run \`needle help\` for available commands.
EOF
        _needle_success "Created README"
    fi

    # -------------------------------------------------------------------------
    # Step 5: Show completion message
    # -------------------------------------------------------------------------
    _needle_print ""

    # Check workspace status
    if [[ -n "$NEEDLE_CONFIG_WORKSPACE" ]] && ! _needle_is_valid_workspace "$NEEDLE_CONFIG_WORKSPACE"; then
        _needle_warn "Your workspace does not have a .beads directory"
        _needle_print ""
        _needle_info "To initialize bead tracking in your workspace, run:"
        _needle_print "    cd \"$NEEDLE_CONFIG_WORKSPACE\" && br init"
        _needle_print ""
    fi

    # Show completion message with quick start guide
    _needle_welcome_complete
}
