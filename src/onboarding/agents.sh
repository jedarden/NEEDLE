#!/usr/bin/env bash
# NEEDLE CLI Agent Detection Module
# Detects and manages coding CLI agents

# -----------------------------------------------------------------------------
# Agent Registry
# -----------------------------------------------------------------------------

# Supported agent commands
declare -A NEEDLE_AGENT_CMDS=(
    [claude]="claude"
    [opencode]="opencode"
    [codex]="codex"
    [aider]="aider"
)

# Installation instructions for each agent
declare -A NEEDLE_AGENT_INSTALL=(
    [claude]="npm install -g @anthropic-ai/claude-code"
    [opencode]="go install github.com/opencode-ai/opencode@latest"
    [codex]="npm install -g @openai/codex"
    [aider]="pip install aider-chat"
)

# Authentication environment variables for each agent
declare -A NEEDLE_AGENT_AUTH_ENV=(
    [claude]="ANTHROPIC_API_KEY"
    [opencode]="OPENAI_API_KEY"
    [codex]="OPENAI_API_KEY"
    [aider]="OPENAI_API_KEY"
)

# Agent display names
declare -A NEEDLE_AGENT_NAMES=(
    [claude]="Claude Code"
    [opencode]="OpenCode"
    [codex]="OpenAI Codex"
    [aider]="Aider"
)

# -----------------------------------------------------------------------------
# Agent Detection Functions
# -----------------------------------------------------------------------------

# Detect if an agent is installed and get its status
# Usage: _needle_detect_agent <agent_name>
# Returns: "missing" or "version|auth_status"
# Exit code: 0 if found, 1 if missing
_needle_detect_agent() {
    local agent="$1"
    local cmd="${NEEDLE_AGENT_CMDS[$agent]}"

    if [[ -z "$cmd" ]]; then
        _needle_error "Unknown agent: $agent"
        return 1
    fi

    if ! command -v "$cmd" &>/dev/null; then
        echo "missing"
        return 1
    fi

    # Get version
    local version
    version=$(_needle_agent_version "$agent")

    # Check auth
    local auth
    auth=$(_needle_agent_auth_status "$agent")

    echo "${version:-unknown}|${auth:-unknown}"
    return 0
}

# Get the version of an installed agent
# Usage: _needle_agent_version <agent_name>
# Returns: version string (e.g., "1.2.3")
_needle_agent_version() {
    local agent="$1"
    local version=""

    case "$agent" in
        claude)
            # claude --version outputs like "claude version 1.2.3"
            version=$(claude --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
            ;;
        opencode)
            # opencode --version outputs like "opencode version 1.2.3" or just "1.2.3"
            version=$(opencode --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -1)
            ;;
        codex)
            # codex --version outputs like "codex/1.2.3" or "1.2.3"
            version=$(codex --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
            ;;
        aider)
            # aider --version outputs like "aider-chat 1.2.3"
            version=$(aider --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
            ;;
        *)
            # Generic fallback: try --version or -V
            version=$("${NEEDLE_AGENT_CMDS[$agent]}" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -1)
            if [[ -z "$version" ]]; then
                version=$("${NEEDLE_AGENT_CMDS[$agent]}" -V 2>/dev/null | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -1)
            fi
            ;;
    esac

    echo "${version:-unknown}"
}

# Check authentication status for an agent
# Usage: _needle_agent_auth_status <agent_name>
# Returns: "authenticated", "auth-required", or "unknown"
_needle_agent_auth_status() {
    local agent="$1"

    case "$agent" in
        claude)
            if claude auth status 2>/dev/null | grep -qi "logged in"; then
                echo "authenticated"
            elif claude auth status 2>/dev/null | grep -qi "not logged in"; then
                echo "auth-required"
            else
                # Fallback: check for API key in environment
                if [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
                    echo "authenticated"
                else
                    echo "auth-required"
                fi
            fi
            ;;
        opencode)
            # opencode uses config file or env var
            if opencode config show 2>/dev/null | grep -qi "api_key"; then
                echo "authenticated"
            elif [[ -n "${OPENAI_API_KEY:-}" ]]; then
                echo "authenticated"
            else
                echo "auth-required"
            fi
            ;;
        codex)
            # codex auth status or env var check
            if codex auth status 2>/dev/null | grep -qi "logged in\|authenticated"; then
                echo "authenticated"
            elif [[ -n "${OPENAI_API_KEY:-}" ]]; then
                echo "authenticated"
            else
                echo "auth-required"
            fi
            ;;
        aider)
            # aider uses environment variables for auth
            if [[ -n "${OPENAI_API_KEY:-}" ]] || [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
                echo "authenticated"
            else
                echo "auth-required"
            fi
            ;;
        *)
            # Generic fallback: check for common API key env vars
            local auth_env="${NEEDLE_AGENT_AUTH_ENV[$agent]:-}"
            if [[ -n "$auth_env" ]] && [[ -n "${!auth_env:-}" ]]; then
                echo "authenticated"
            else
                echo "auth-required"
            fi
            ;;
    esac
}

# Get installation instruction for an agent
# Usage: _needle_agent_install_cmd <agent_name>
# Returns: installation command string
_needle_agent_install_cmd() {
    local agent="$1"
    echo "${NEEDLE_AGENT_INSTALL[$agent]:-echo 'No install command available'}"
}

# -----------------------------------------------------------------------------
# Agent Scanning Functions
# -----------------------------------------------------------------------------

# Scan all known agents and report their status
# Usage: _needle_scan_agents [--json]
# Output: Human-readable or JSON format
_needle_scan_agents() {
    local json_output=false
    local found=0
    local total=${#NEEDLE_AGENT_CMDS[@]}

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --json|-j)
                json_output=true
                shift
                ;;
            *)
                shift
                ;;
        esac
    done

    if [[ "$json_output" == "true" ]]; then
        _needle_scan_agents_json
        return $?
    fi

    # Human-readable output
    _needle_section "Agent Detection"
    _needle_info "Scanning for coding CLI agents..."
    _needle_print ""

    local agent_order=("claude" "opencode" "codex" "aider")

    for agent in "${agent_order[@]}"; do
        local result
        result=$(_needle_detect_agent "$agent" 2>/dev/null)
        local exit_code=$?

        local name="${NEEDLE_AGENT_NAMES[$agent]:-$agent}"

        if [[ "$result" == "missing" ]] || [[ $exit_code -ne 0 ]]; then
            _needle_warn "$name not found"
            _needle_print "      └─ Install: ${NEEDLE_AGENT_INSTALL[$agent]}"
        else
            IFS='|' read -r version auth <<< "$result"
            _needle_success "$name $version"

            case "$auth" in
                authenticated)
                    _needle_print "      └─ Auth: ${NEEDLE_COLOR_GREEN}authenticated${NEEDLE_COLOR_RESET}"
                    ;;
                auth-required)
                    _needle_print "      └─ Auth: ${NEEDLE_COLOR_YELLOW}auth required${NEEDLE_COLOR_RESET}"
                    ;;
                *)
                    _needle_print "      └─ Auth: ${NEEDLE_COLOR_DIM}$auth${NEEDLE_COLOR_RESET}"
                    ;;
            esac
            ((found++))
        fi
        _needle_print ""
    done

    # Summary
    if [[ $found -eq 0 ]]; then
        _needle_warn "No agents found. Install at least one agent to get started."
        _needle_print ""
        _needle_info "Recommended: claude (Claude Code)"
        _needle_print "      ${NEEDLE_AGENT_INSTALL[claude]}"
    elif [[ $found -lt $total ]]; then
        _needle_info "$found of $total agents ready"
    else
        _needle_success "All $found agents ready"
    fi

    return 0
}

# Scan agents and output as JSON
# Usage: _needle_scan_agents_json
# Output: JSON array of agent statuses
_needle_scan_agents_json() {
    local agents_json="["
    local first=true

    local agent_order=("claude" "opencode" "codex" "aider")

    for agent in "${agent_order[@]}"; do
        local result
        result=$(_needle_detect_agent "$agent" 2>/dev/null)

        if [[ "$first" == "true" ]]; then
            first=false
        else
            agents_json+=","
        fi

        if [[ "$result" == "missing" ]]; then
            agents_json+="{"
            agents_json+="\"name\":\"$(_needle_json_escape "$agent")\","
            agents_json+="\"display_name\":\"$(_needle_json_escape "${NEEDLE_AGENT_NAMES[$agent]:-$agent}")\","
            agents_json+="\"installed\":false,"
            agents_json+="\"version\":null,"
            agents_json+="\"auth_status\":null,"
            agents_json+="\"install_command\":\"$(_needle_json_escape "${NEEDLE_AGENT_INSTALL[$agent]}")\""
            agents_json+="}"
        else
            IFS='|' read -r version auth <<< "$result"
            agents_json+="{"
            agents_json+="\"name\":\"$(_needle_json_escape "$agent")\","
            agents_json+="\"display_name\":\"$(_needle_json_escape "${NEEDLE_AGENT_NAMES[$agent]:-$agent}")\","
            agents_json+="\"installed\":true,"
            agents_json+="\"version\":\"$(_needle_json_escape "$version")\","
            agents_json+="\"auth_status\":\"$(_needle_json_escape "$auth")\","
            agents_json+="\"install_command\":\"$(_needle_json_escape "${NEEDLE_AGENT_INSTALL[$agent]}")\""
            agents_json+="}"
        fi
    done

    agents_json+="]"
    echo "$agents_json"
}

# Get list of installed agents
# Usage: _needle_get_installed_agents
# Returns: space-separated list of installed agent names
_needle_get_installed_agents() {
    local installed=""

    for agent in "${!NEEDLE_AGENT_CMDS[@]}"; do
        local cmd="${NEEDLE_AGENT_CMDS[$agent]}"
        if command -v "$cmd" &>/dev/null; then
            if [[ -n "$installed" ]]; then
                installed+=" $agent"
            else
                installed="$agent"
            fi
        fi
    done

    echo "$installed"
}

# Get list of authenticated agents
# Usage: _needle_get_authenticated_agents
# Returns: space-separated list of authenticated agent names
_needle_get_authenticated_agents() {
    local authenticated=""

    for agent in "${!NEEDLE_AGENT_CMDS[@]}"; do
        local result
        result=$(_needle_detect_agent "$agent" 2>/dev/null)
        if [[ "$result" != "missing" ]]; then
            IFS='|' read -r version auth <<< "$result"
            if [[ "$auth" == "authenticated" ]]; then
                if [[ -n "$authenticated" ]]; then
                    authenticated+=" $agent"
                else
                    authenticated="$agent"
                fi
            fi
        fi
    done

    echo "$authenticated"
}

# Check if a specific agent is ready (installed and authenticated)
# Usage: _needle_is_agent_ready <agent_name>
# Returns: 0 if ready, 1 otherwise
_needle_is_agent_ready() {
    local agent="$1"
    local result
    result=$(_needle_detect_agent "$agent" 2>/dev/null)

    if [[ "$result" == "missing" ]]; then
        return 1
    fi

    IFS='|' read -r version auth <<< "$result"
    if [[ "$auth" == "authenticated" ]]; then
        return 0
    fi

    return 1
}

# Get the first ready agent (useful for default selection)
# Usage: _needle_get_default_agent
# Returns: name of first ready agent, or empty string if none ready
_needle_get_default_agent() {
    local preference_order=("claude" "aider" "opencode" "codex")

    for agent in "${preference_order[@]}"; do
        if _needle_is_agent_ready "$agent"; then
            echo "$agent"
            return 0
        fi
    done

    # Fallback: return first installed (even if not authenticated)
    for agent in "${preference_order[@]}"; do
        local cmd="${NEEDLE_AGENT_CMDS[$agent]}"
        if command -v "$cmd" &>/dev/null; then
            echo "$agent"
            return 0
        fi
    done

    return 1
}

# Print detailed agent information
# Usage: _needle_agent_info <agent_name>
_needle_agent_info() {
    local agent="$1"
    local name="${NEEDLE_AGENT_NAMES[$agent]:-$agent}"
    local cmd="${NEEDLE_AGENT_CMDS[$agent]:-}"

    if [[ -z "$cmd" ]]; then
        _needle_error "Unknown agent: $agent"
        return 1
    fi

    _needle_header "$name"

    # Check if installed
    if ! command -v "$cmd" &>/dev/null; then
        _needle_warn "Not installed"
        _needle_print ""
        _needle_info "Install with:"
        _needle_print "  ${NEEDLE_AGENT_INSTALL[$agent]}"
        return 1
    fi

    # Get version
    local version
    version=$(_needle_agent_version "$agent")
    _needle_table_row "Version" "$version"

    # Get auth status
    local auth
    auth=$(_needle_agent_auth_status "$agent")
    _needle_table_row "Auth" "$auth"

    # Get command path
    local cmd_path
    cmd_path=$(command -v "$cmd")
    _needle_table_row "Path" "$cmd_path"

    # Show relevant env var
    local auth_env="${NEEDLE_AGENT_AUTH_ENV[$agent]}"
    if [[ -n "$auth_env" ]]; then
        local env_status="not set"
        if [[ -n "${!auth_env:-}" ]]; then
            env_status="set (${#auth_env} chars)"
        fi
        _needle_table_row "Env ($auth_env)" "$env_status"
    fi

    return 0
}
