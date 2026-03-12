#!/usr/bin/env bash
# NEEDLE CLI Welcome Module
# ASCII banner and welcome message for onboarding

# -----------------------------------------------------------------------------
# Banner Configuration
# -----------------------------------------------------------------------------

# Banner dimensions
NEEDLE_BANNER_WIDTH=67

# -----------------------------------------------------------------------------
# Banner Display Functions
# -----------------------------------------------------------------------------

# Display the NEEDLE ASCII art banner
# Uses cyan color when terminal supports it
# Respects NO_COLOR environment variable
_needle_show_banner() {
    # Apply cyan color to the banner box if color is enabled
    if [[ -n "$NEEDLE_COLOR_CYAN" ]]; then
        printf '%b' "$NEEDLE_COLOR_CYAN"
    fi

    cat <<'BANNER'
╔═══════════════════════════════════════════════════════════════╗
║                                                               ║
║   ███╗   ██╗███████╗███████╗██████╗ ██╗     ███████╗         ║
║   ████╗  ██║██╔════╝██╔════╝██╔══██╗██║     ██╔════╝         ║
║   ██╔██╗ ██║█████╗  █████╗  ██║  ██║██║     █████╗           ║
║   ██║╚██╗██║██╔══╝  ██╔══╝  ██║  ██║██║     ██╔══╝           ║
║   ██║ ╚████║███████╗███████╗██████╔╝███████╗███████╗         ║
║   ╚═╝  ╚═══╝╚══════╝╚══════╝╚═════╝ ╚══════╝╚══════╝         ║
║                                                               ║
BANNER

    # Reset color before tagline so we can style it separately
    if [[ -n "$NEEDLE_COLOR_RESET" ]]; then
        printf '%b' "$NEEDLE_COLOR_RESET"
    fi

    # Print tagline with emphasis on NEEDLE (bold if color enabled)
    if [[ -n "$NEEDLE_COLOR_BOLD" ]]; then
        printf '║   %bNavigates Every Enqueued Deliverable, Logs Effort%b          ║\n' "$NEEDLE_COLOR_BOLD" "$NEEDLE_COLOR_RESET"
    else
        printf '║   Navigates Every Enqueued Deliverable, Logs Effort          ║\n'
    fi

    # Re-apply cyan for bottom border
    if [[ -n "$NEEDLE_COLOR_CYAN" ]]; then
        printf '%b' "$NEEDLE_COLOR_CYAN"
    fi

    printf '║                                                               ║\n'
    printf '╚═══════════════════════════════════════════════════════════════╝\n'

    # Reset color after banner
    if [[ -n "$NEEDLE_COLOR_RESET" ]]; then
        printf '%b' "$NEEDLE_COLOR_RESET"
    fi

    # Welcome message
    printf '\n'
    printf '    Welcome to '
    if [[ -n "$NEEDLE_COLOR_BOLD" ]]; then
        printf '%bNEEDLE%b' "$NEEDLE_COLOR_BOLD" "$NEEDLE_COLOR_RESET"
    else
        printf 'NEEDLE'
    fi
    printf "! Let's get you set up.\n"
    printf '\n'
}

# Display a minimal banner (for non-interactive use)
_needle_show_banner_minimal() {
    if [[ -n "$NEEDLE_COLOR_BOLD" ]]; then
        printf '%bNEEDLE%b - Navigates Every Enqueued Deliverable, Logs Effort\n' "$NEEDLE_COLOR_BOLD" "$NEEDLE_COLOR_RESET"
    else
        printf 'NEEDLE - Navigates Every Enqueued Deliverable, Logs Effort\n'
    fi
}

# Display welcome message for init command
_needle_welcome_init() {
    _needle_show_banner

    if [[ "${NEEDLE_QUIET:-}" == "true" ]]; then
        return
    fi

    _needle_info "This setup will guide you through:"
    _needle_print "    1. Installing dependencies (tmux, jq, yq, br)"
    _needle_print "    2. Detecting available coding CLI agents"
    _needle_print "    3. Configuring your first workspace"
    _needle_print "    4. Creating default configuration"
    _needle_print ""
}

# Display post-initialization success message
_needle_welcome_complete() {
    if [[ "${NEEDLE_QUIET:-}" == "true" ]]; then
        return
    fi

    _needle_print ""
    _needle_success "NEEDLE is ready to go!"
    _needle_print ""

    if [[ -n "$NEEDLE_COLOR_CYAN" ]]; then
        printf '%b' "$NEEDLE_COLOR_CYAN"
    fi

    cat <<'QUICKSTART'
    ┌─────────────────────────────────────────────────────────────┐
    │                     Quick Start Guide                       │
    ├─────────────────────────────────────────────────────────────┤
    │                                                             │
    │   needle run <task>     Start a new task                   │
    │   needle list           Show running workers               │
    │   needle status         View system health                 │
    │   needle help           See all commands                   │
    │                                                             │
    └─────────────────────────────────────────────────────────────┘
QUICKSTART

    if [[ -n "$NEEDLE_COLOR_RESET" ]]; then
        printf '%b' "$NEEDLE_COLOR_RESET"
    fi

    _needle_print ""
    _needle_info "Configuration: $NEEDLE_CONFIG_FILE"
    _needle_print ""
}
