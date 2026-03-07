#!/usr/bin/env bash
# NEEDLE Agent Settings Hook Injection
# Manages injection of file-checkout hooks into agent settings.json
#
# Claude Code supports hooks via settings.json:
# {
#   "hooks": {
#     "preToolUse": [
#       { "matcher": "Edit|Write", "hook": "~/.needle/hooks/file-checkout.sh" }
#     ]
#   }
# }
#
# This module provides functions to inject, update, and remove hooks
# from agent settings files.

# ============================================================================
# Claude Code Settings Paths
# ============================================================================

# Default Claude Code settings paths (in order of precedence)
NEEDLE_CLAUDE_SETTINGS_PATHS=(
    "$HOME/.claude/settings.json"
    "$HOME/.config/claude/settings.json"
    "$HOME/.claude.json"
)

# ============================================================================
# Utility Functions
# ============================================================================

# Find Claude Code settings file
# Usage: _needle_find_claude_settings
# Returns: Path to settings file, or empty if not found
_needle_find_claude_settings() {
    for path in "${NEEDLE_CLAUDE_SETTINGS_PATHS[@]}"; do
        if [[ -f "$path" ]]; then
            echo "$path"
            return 0
        fi
    done
    return 1
}

# Check if jq is available for JSON manipulation
_needle_has_jq() {
    command -v jq &>/dev/null
}

# ============================================================================
# Hook Injection Functions
# ============================================================================

# Inject file-checkout hook into Claude Code settings
# Usage: _needle_inject_file_checkout_hook [settings_path]
# Arguments:
#   settings_path - Optional path to settings.json (auto-detected if not provided)
# Returns: 0 on success, 1 on failure
_needle_inject_file_checkout_hook() {
    local settings_path="${1:-}"
    local hook_script="$HOME/.needle/hooks/file-checkout.sh"
    local hook_matcher="Edit|Write"

    # Find settings file if not provided
    if [[ -z "$settings_path" ]]; then
        settings_path=$(_needle_find_claude_settings) || {
            _needle_warn "Claude Code settings.json not found"
            _needle_info "Will create at: ${NEEDLE_CLAUDE_SETTINGS_PATHS[0]}"
            settings_path="${NEEDLE_CLAUDE_SETTINGS_PATHS[0]}"
        }
    fi

    # Ensure hook script exists
    if [[ ! -f "$hook_script" ]]; then
        # Try to find it in NEEDLE installation
        local needle_hook="${NEEDLE_SRC_DIR:-}/hooks/file-checkout.sh"
        if [[ -f "$needle_hook" ]]; then
            # Copy to ~/.needle/hooks/
            mkdir -p "$(dirname "$hook_script")"
            cp "$needle_hook" "$hook_script"
            chmod +x "$hook_script"
            _needle_info "Installed hook script to: $hook_script"
        else
            _needle_error "Hook script not found: $hook_script"
            return 1
        fi
    fi

    # Make hook executable
    chmod +x "$hook_script" 2>/dev/null

    # Check if jq is available
    if ! _needle_has_jq; then
        _needle_error "jq is required for hook injection"
        _needle_info "Install with: apt install jq || brew install jq"
        return 1
    fi

    # Create settings directory if needed
    local settings_dir
    settings_dir=$(dirname "$settings_path")
    if [[ ! -d "$settings_dir" ]]; then
        mkdir -p "$settings_dir" || {
            _needle_error "Failed to create directory: $settings_dir"
            return 1
        }
    fi

    # Read existing settings or create empty object
    local settings="{}"
    if [[ -f "$settings_path" ]]; then
        settings=$(cat "$settings_path" 2>/dev/null || echo "{}")
    fi

    # Check if hook already exists
    local existing_hook
    existing_hook=$(echo "$settings" | jq -r --arg hook "$hook_script" '
        .hooks.preToolUse // [] | map(select(.hook == $hook)) | length
    ' 2>/dev/null || echo "0")

    if [[ "$existing_hook" -gt 0 ]]; then
        _needle_info "File-checkout hook already installed in: $settings_path"
        return 0
    fi

    # Inject the hook
    local updated_settings
    updated_settings=$(echo "$settings" | jq --arg matcher "$hook_matcher" --arg hook "$hook_script" '
        .hooks.preToolUse = (.hooks.preToolUse // []) + [{
            "matcher": $matcher,
            "hook": $hook
        }]
    ' 2>/dev/null)

    if [[ -z "$updated_settings" ]] || [[ "$updated_settings" == "null" ]]; then
        _needle_error "Failed to update settings JSON"
        return 1
    fi

    # Write updated settings
    if ! echo "$updated_settings" > "$settings_path" 2>/dev/null; then
        _needle_error "Failed to write settings: $settings_path"
        return 1
    fi

    _needle_success "Injected file-checkout hook into: $settings_path"
    _needle_info "Hook will intercept Edit and Write tool calls"
    return 0
}

# Remove file-checkout hook from Claude Code settings
# Usage: _needle_remove_file_checkout_hook [settings_path]
# Returns: 0 on success, 1 on failure
_needle_remove_file_checkout_hook() {
    local settings_path="${1:-}"
    local hook_script="$HOME/.needle/hooks/file-checkout.sh"

    # Find settings file if not provided
    if [[ -z "$settings_path" ]]; then
        settings_path=$(_needle_find_claude_settings) || {
            _needle_warn "Claude Code settings.json not found"
            return 0
        }
    fi

    if [[ ! -f "$settings_path" ]]; then
        _needle_info "Settings file not found: $settings_path"
        return 0
    fi

    # Check if jq is available
    if ! _needle_has_jq; then
        _needle_error "jq is required for hook removal"
        return 1
    fi

    # Read existing settings
    local settings
    settings=$(cat "$settings_path" 2>/dev/null || echo "{}")

    # Remove the hook
    local updated_settings
    updated_settings=$(echo "$settings" | jq --arg hook "$hook_script" '
        .hooks.preToolUse = (.hooks.preToolUse // []) | map(select(.hook != $hook))
    ' 2>/dev/null)

    # Write updated settings
    if ! echo "$updated_settings" > "$settings_path" 2>/dev/null; then
        _needle_error "Failed to write settings: $settings_path"
        return 1
    fi

    _needle_success "Removed file-checkout hook from: $settings_path"
    return 0
}

# Check if file-checkout hook is installed
# Usage: _needle_is_file_checkout_hook_installed [settings_path]
# Returns: 0 if installed, 1 if not
_needle_is_file_checkout_hook_installed() {
    local settings_path="${1:-}"
    local hook_script="$HOME/.needle/hooks/file-checkout.sh"

    # Find settings file if not provided
    if [[ -z "$settings_path" ]]; then
        settings_path=$(_needle_find_claude_settings) || return 1
    fi

    if [[ ! -f "$settings_path" ]]; then
        return 1
    fi

    # Check if jq is available
    if ! _needle_has_jq; then
        return 1
    fi

    # Check for hook
    local count
    count=$(jq -r --arg hook "$hook_script" '
        .hooks.preToolUse // [] | map(select(.hook == $hook)) | length
    ' < "$settings_path" 2>/dev/null || echo "0")

    [[ "$count" -gt 0 ]]
}

# ============================================================================
# Hook Status and Management
# ============================================================================

# Show status of file-checkout hook installation
# Usage: _needle_file_checkout_hook_status
_needle_file_checkout_hook_status() {
    _needle_section "File Checkout Hook Status"

    local hook_script="$HOME/.needle/hooks/file-checkout.sh"

    # Check hook script
    _needle_print "Hook Script:"
    if [[ -f "$hook_script" ]]; then
        if [[ -x "$hook_script" ]]; then
            _needle_table_row "  Status" "${NEEDLE_COLOR_GREEN}installed (executable)${NEEDLE_COLOR_RESET}"
        else
            _needle_table_row "  Status" "${NEEDLE_COLOR_YELLOW}installed (not executable)${NEEDLE_COLOR_RESET}"
        fi
        _needle_table_row "  Path" "$hook_script"
    else
        _needle_table_row "  Status" "${NEEDLE_COLOR_RED}not installed${NEEDLE_COLOR_RESET}"
    fi

    _needle_print ""
    _needle_print "Claude Code Settings:"

    for path in "${NEEDLE_CLAUDE_SETTINGS_PATHS[@]}"; do
        if [[ -f "$path" ]]; then
            if _needle_is_file_checkout_hook_installed "$path"; then
                _needle_table_row "  $path" "${NEEDLE_COLOR_GREEN}hook installed${NEEDLE_COLOR_RESET}"
            else
                _needle_table_row "  $path" "${NEEDLE_COLOR_YELLOW}no hook${NEEDLE_COLOR_RESET}"
            fi
        fi
    done

    _needle_print ""
    _needle_print "Lock Directory:"
    local lock_dir="/dev/shm/needle"
    if [[ -d "$lock_dir" ]]; then
        local lock_count
        lock_count=$(ls -1 "$lock_dir" 2>/dev/null | wc -l)
        _needle_table_row "  Status" "${NEEDLE_COLOR_GREEN}active${NEEDLE_COLOR_RESET}"
        _needle_table_row "  Active Locks" "$lock_count"
        _needle_table_row "  Path" "$lock_dir"
    else
        _needle_table_row "  Status" "${NEEDLE_COLOR_YELLOW}not initialized${NEEDLE_COLOR_RESET}"
        _needle_table_row "  Path" "$lock_dir (will be created on first use)"
    fi
}

# Install file-checkout hook (main entry point)
# Usage: _needle_install_file_checkout_hook [--force]
# Options:
#   --force  Reinstall even if already installed
_needle_install_file_checkout_hook() {
    local force=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --force)
                force=true
                shift
                ;;
            *)
                shift
                ;;
        esac
    done

    if [[ "$force" == "true" ]]; then
        _needle_remove_file_checkout_hook 2>/dev/null || true
    fi

    _needle_inject_file_checkout_hook
}

# ============================================================================
# Setup Command Integration
# ============================================================================

# Setup file-checkout system (called by needle setup)
# Usage: _needle_setup_file_checkout
_needle_setup_file_checkout() {
    _needle_section "File Checkout System"

    # Ensure lock module is available
    local lock_module="${NEEDLE_SRC_DIR:-}/lock/checkout.sh"
    if [[ ! -f "$lock_module" ]]; then
        _needle_error "Lock module not found: $lock_module"
        return 1
    fi

    # Create hooks directory
    local hooks_dir="$HOME/.needle/hooks"
    if [[ ! -d "$hooks_dir" ]]; then
        mkdir -p "$hooks_dir" || {
            _needle_error "Failed to create hooks directory: $hooks_dir"
            return 1
        }
        _needle_info "Created hooks directory: $hooks_dir"
    fi

    # Install hook script
    local hook_script="$hooks_dir/file-checkout.sh"
    if [[ -f "$hook_script" ]] && [[ "$force" != "true" ]]; then
        _needle_info "Hook script already installed: $hook_script"
    else
        # Copy from source if available
        local src_hook="${NEEDLE_SRC_DIR:-}/hooks/file-checkout.sh"
        if [[ -f "$src_hook" ]]; then
            cp "$src_hook" "$hook_script"
            chmod +x "$hook_script"
            _needle_success "Installed hook script: $hook_script"
        else
            _needle_error "Source hook script not found: $src_hook"
            return 1
        fi
    fi

    # Inject into Claude Code settings
    if _needle_is_file_checkout_hook_installed; then
        _needle_info "Hook already injected in Claude Code settings"
    else
        if _needle_inject_file_checkout_hook; then
            _needle_success "Injected hook into Claude Code settings"
        else
            _needle_warn "Could not inject hook into Claude Code settings"
            _needle_info "You may need to manually add to ~/.claude/settings.json:"
            _needle_print '  "hooks": {'
            _needle_print '    "preToolUse": ['
            _needle_print '      { "matcher": "Edit|Write", "hook": "~/.needle/hooks/file-checkout.sh" }'
            _needle_print '    ]'
            _needle_print '  }'
        fi
    fi

    return 0
}
