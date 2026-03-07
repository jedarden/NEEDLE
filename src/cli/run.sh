#!/usr/bin/env bash
# NEEDLE CLI Run Subcommand
# Execute a needle workflow or script
#
# This module implements CLI parsing and validation for the 'needle run' command.
# It parses command-line arguments, validates them, applies config defaults,
# and exports validated options for the worker execution phase.
#
# Multi-worker spawning (nd-2pw):
# - Handles --count=N option to spawn multiple workers
# - Uses NATO alphabet identifiers via naming.sh module
# - Creates tmux sessions via tmux.sh module
# - Reports all session names at end
# - Respects concurrency limits from limits.sh module

# -----------------------------------------------------------------------------
# Run Command Help
# -----------------------------------------------------------------------------

_needle_run_help() {
    _needle_print "Start a worker to process beads from the queue

Launches a NEEDLE worker that claims and executes beads. The worker
runs in a tmux session for persistence and can be attached/detached.

USAGE:
    needle run [OPTIONS]

OPTIONS:
    -w, --workspace <PATH>   Workspace directory to process beads from
                             [default: current directory or config default]

    -a, --agent <NAME>       Agent adapter to use for execution
                             [default: from config]

    -i, --id <ID>            Worker identifier (e.g., \"alpha\", \"primary\")
                             [default: auto-assigned NATO alphabet]

    -c, --count <N>          Number of workers to spawn [default: 1]

    --no-tmux                Run directly without tmux (for debugging)
    --foreground             Run in foreground, don't detach

    --strands <LIST>         Comma-separated strands to enable
                             [default: from config]

    -h, --help               Print help information

STRANDS:
    1. pluck     - Process beads from configured workspaces
    2. explore   - Look for work in other workspaces
    3. mend      - Maintenance and cleanup tasks
    4. weave     - Create beads from documentation gaps (opt-in)
    5. unravel   - Create alternatives for blocked beads (opt-in)
    6. pulse     - Codebase health monitoring (opt-in)
    7. knot      - Alert human when stuck

EXAMPLES:
    # Start a worker with defaults
    needle run

    # Start with explicit workspace and agent
    needle run --workspace=/home/coder/project --agent=claude-anthropic-sonnet

    # Start 3 workers
    needle run --count=3

    # Start with custom identifier
    needle run --id=primary

    # Debug mode (no tmux, foreground)
    needle run --no-tmux --foreground

    # Enable only specific strands
    needle run --strands=pluck,mend

SESSION NAMING:
    Workers are named using the configured pattern:
    needle-{runner}-{provider}-{model}-{identifier}

    Example: needle-claude-anthropic-sonnet-alpha

SEE ALSO:
    needle list      List running workers
    needle attach    Attach to a worker session
    needle stop      Stop worker(s)
"
}

# -----------------------------------------------------------------------------
# Validation Functions
# -----------------------------------------------------------------------------

# Validate workspace path exists and has .beads/ directory
# Usage: _needle_validate_workspace <path>
# Returns: 0 if valid, 1 if invalid
# Sets: NEEDLE_VALIDATED_WORKSPACE to absolute path
_needle_validate_workspace() {
    local workspace="$1"

    # If not specified, use current directory
    if [[ -z "$workspace" ]]; then
        workspace="$(pwd)"
    fi

    # Resolve to absolute path
    local abs_path
    abs_path="$(cd "$workspace" 2>/dev/null && pwd)" || {
        _needle_error "Workspace not found: $workspace"
        return 1
    }

    # Check if directory exists
    if [[ ! -d "$abs_path" ]]; then
        _needle_error "Workspace is not a directory: $abs_path"
        return 1
    fi

    # Check for .beads/ directory
    if [[ ! -d "$abs_path/.beads" ]]; then
        _needle_error "Workspace missing .beads/ directory: $abs_path"
        _needle_info "Run 'needle init' in the workspace to initialize it"
        return 1
    fi

    # Valid - store and export
    NEEDLE_VALIDATED_WORKSPACE="$abs_path"
    export NEEDLE_VALIDATED_WORKSPACE
    return 0
}

# Validate agent exists and is configured
# Usage: _needle_validate_agent <agent_name>
# Returns: 0 if valid, 1 if invalid
# Sets: NEEDLE_VALIDATED_AGENT to agent name
_needle_validate_agent() {
    local agent="$1"

    # If not specified, try to get default
    if [[ -z "$agent" ]]; then
        agent="$(_needle_get_default_agent 2>/dev/null)"
        if [[ -z "$agent" ]]; then
            _needle_error "No agent specified and no default agent found"
            _needle_info "Specify an agent with --agent or run 'needle agents scan' to detect available agents"
            return 1
        fi
        _needle_verbose "Using default agent: $agent"
    fi

    # Check if agent config exists
    if ! _needle_find_agent_config "$agent" &>/dev/null; then
        _needle_error "Agent not found: $agent"
        _needle_info "Run 'needle agents list' to see available agents"
        return 1
    fi

    # Validate agent configuration
    if ! _needle_load_agent "$agent" &>/dev/null; then
        _needle_error "Failed to load agent configuration: $agent"
        return 1
    fi

    # Valid - store and export
    NEEDLE_VALIDATED_AGENT="$agent"
    export NEEDLE_VALIDATED_AGENT
    return 0
}

# Validate worker count is positive integer
# Usage: _needle_validate_count <count>
# Returns: 0 if valid, 1 if invalid
# Sets: NEEDLE_VALIDATED_COUNT to count
_needle_validate_count() {
    local count="$1"

    # If not specified, use default
    if [[ -z "$count" ]]; then
        count="1"
    fi

    # Must be a positive integer
    if [[ ! "$count" =~ ^[1-9][0-9]*$ ]]; then
        _needle_error "Invalid worker count: $count (must be a positive integer)"
        return 1
    fi

    # Valid - store and export
    NEEDLE_VALIDATED_COUNT="$count"
    export NEEDLE_VALIDATED_COUNT
    return 0
}

# Validate budget is positive number
# Usage: _needle_validate_budget <budget>
# Returns: 0 if valid, 1 if invalid
# Sets: NEEDLE_VALIDATED_BUDGET to budget (or empty if not specified)
_needle_validate_budget() {
    local budget="$1"

    # Budget is optional
    if [[ -z "$budget" ]]; then
        NEEDLE_VALIDATED_BUDGET=""
        export NEEDLE_VALIDATED_BUDGET
        return 0
    fi

    # Must be a positive number (integer or decimal)
    # First check format, then check it's greater than 0
    if [[ ! "$budget" =~ ^[0-9]+\.?[0-9]*$ ]]; then
        _needle_error "Invalid budget: $budget (must be a positive number)"
        return 1
    fi

    # Check that it's greater than 0
    # Handle both integer and decimal comparison
    local is_zero=false
    if [[ "$budget" == "0" ]] || [[ "$budget" == "0.0" ]] || [[ "$budget" =~ ^0\.0+$ ]]; then
        is_zero=true
    fi

    if [[ "$is_zero" == "true" ]]; then
        _needle_error "Invalid budget: $budget (must be greater than 0)"
        return 1
    fi

    # Valid - store and export
    NEEDLE_VALIDATED_BUDGET="$budget"
    export NEEDLE_VALIDATED_BUDGET
    return 0
}

# -----------------------------------------------------------------------------
# Config Defaults Application
# -----------------------------------------------------------------------------

# Apply configuration defaults for missing options
# Uses workspace config if available, falls back to global config
# Usage: _needle_apply_config_defaults
# Sets: NEEDLE_DEFAULT_AGENT, NEEDLE_DEFAULT_BUDGET
_needle_apply_config_defaults() {
    local workspace="${NEEDLE_VALIDATED_WORKSPACE:-$(pwd)}"

    # Get default agent from workspace config or global config
    if [[ -z "${NEEDLE_VALIDATED_AGENT:-}" ]]; then
        local config_agent
        config_agent=$(get_workspace_setting "$workspace" "runner.default_agent" "" 2>/dev/null)
        if [[ -n "$config_agent" ]]; then
            NEEDLE_DEFAULT_AGENT="$config_agent"
            export NEEDLE_DEFAULT_AGENT
            _needle_verbose "Config default agent: $config_agent"
        fi
    fi

    # Get default budget from config
    if [[ -z "${NEEDLE_VALIDATED_BUDGET:-}" ]]; then
        local config_budget
        config_budget=$(get_workspace_setting "$workspace" "effort.budget.daily_limit_usd" "" 2>/dev/null)
        if [[ -n "$config_budget" ]] && [[ "$config_budget" != "null" ]]; then
            NEEDLE_DEFAULT_BUDGET="$config_budget"
            export NEEDLE_DEFAULT_BUDGET
            _needle_verbose "Config default budget: \$$config_budget"
        fi
    fi

    # Get default worker count from config
    if [[ -z "${NEEDLE_VALIDATED_COUNT:-}" ]] || [[ "${NEEDLE_VALIDATED_COUNT:-}" == "1" ]]; then
        local config_count
        config_count=$(get_workspace_setting "$workspace" "runner.default_workers" "" 2>/dev/null)
        if [[ -n "$config_count" ]] && [[ "$config_count" != "null" ]]; then
            NEEDLE_DEFAULT_COUNT="$config_count"
            export NEEDLE_DEFAULT_COUNT
            _needle_verbose "Config default worker count: $config_count"
        fi
    fi
}

# -----------------------------------------------------------------------------
# CLI Parsing
# -----------------------------------------------------------------------------

# Parse and validate all run options
# Usage: _needle_run_parse_args "$@"
# Returns: 0 if valid, exits with error code if invalid
# Exports all NEEDLE_VALIDATED_* variables
_needle_run_parse_args() {
    local workspace=""
    local agent=""
    local count=""
    local budget=""
    local session_name=""
    local no_hooks=false
    local dry_run=false
    local force=false
    local wait_for_slot=false
    local show_status=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -w|--workspace)
                if [[ -z "${2:-}" ]]; then
                    _needle_error "Option $1 requires an argument"
                    exit $NEEDLE_EXIT_USAGE
                fi
                workspace="$2"
                shift 2
                ;;
            --workspace=*)
                workspace="${1#*=}"
                shift
                ;;
            -a|--agent)
                if [[ -z "${2:-}" ]]; then
                    _needle_error "Option $1 requires an argument"
                    exit $NEEDLE_EXIT_USAGE
                fi
                agent="$2"
                shift 2
                ;;
            --agent=*)
                agent="${1#*=}"
                shift
                ;;
            -c|--count)
                if [[ -z "${2:-}" ]]; then
                    _needle_error "Option $1 requires an argument"
                    exit $NEEDLE_EXIT_USAGE
                fi
                count="$2"
                shift 2
                ;;
            --count=*)
                count="${1#*=}"
                shift
                ;;
            -n|--name)
                if [[ -z "${2:-}" ]]; then
                    _needle_error "Option $1 requires an argument"
                    exit $NEEDLE_EXIT_USAGE
                fi
                session_name="$2"
                shift 2
                ;;
            --name=*)
                session_name="${1#*=}"
                shift
                ;;
            --budget)
                if [[ -z "${2:-}" ]]; then
                    _needle_error "Option $1 requires an argument"
                    exit $NEEDLE_EXIT_USAGE
                fi
                budget="$2"
                shift 2
                ;;
            --budget=*)
                budget="${1#*=}"
                shift
                ;;
            --no-hooks)
                no_hooks=true
                shift
                ;;
            -d|--dry-run)
                dry_run=true
                shift
                ;;
            -f|--force)
                force=true
                shift
                ;;
            --wait)
                wait_for_slot=true
                shift
                ;;
            --status)
                show_status=true
                shift
                ;;
            -v|--verbose)
                NEEDLE_VERBOSE=true
                shift
                ;;
            -h|--help)
                _needle_run_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            -*)
                _needle_error "Unknown option: $1"
                _needle_run_help
                exit $NEEDLE_EXIT_USAGE
                ;;
            *)
                # Positional argument - treat as workspace if not set
                if [[ -z "$workspace" ]]; then
                    workspace="$1"
                else
                    _needle_error "Unexpected argument: $1"
                    exit $NEEDLE_EXIT_USAGE
                fi
                shift
                ;;
        esac
    done

    # Store raw parsed values for status display
    NEEDLE_RAW_WORKSPACE="$workspace"
    NEEDLE_RAW_AGENT="$agent"
    NEEDLE_RAW_COUNT="$count"
    NEEDLE_RAW_BUDGET="$budget"
    NEEDLE_RAW_SESSION_NAME="$session_name"
    NEEDLE_RAW_NO_HOOKS="$no_hooks"
    NEEDLE_RAW_DRY_RUN="$dry_run"
    NEEDLE_RAW_FORCE="$force"
    NEEDLE_RAW_WAIT="$wait_for_slot"
    NEEDLE_RAW_STATUS="$show_status"

    # Export flags
    NEEDLE_VALIDATED_NO_HOOKS="$no_hooks"
    NEEDLE_VALIDATED_DRY_RUN="$dry_run"
    NEEDLE_VALIDATED_FORCE="$force"
    NEEDLE_VALIDATED_WAIT="$wait_for_slot"
    NEEDLE_VALIDATED_STATUS="$show_status"
    NEEDLE_VALIDATED_SESSION_NAME="$session_name"
    export NEEDLE_VALIDATED_NO_HOOKS NEEDLE_VALIDATED_DRY_RUN NEEDLE_VALIDATED_FORCE NEEDLE_VALIDATED_WAIT NEEDLE_VALIDATED_STATUS NEEDLE_VALIDATED_SESSION_NAME

    # Validate workspace
    if ! _needle_validate_workspace "$workspace"; then
        exit $NEEDLE_EXIT_USAGE
    fi

    # Apply config defaults before agent validation (agent might be in config)
    _needle_apply_config_defaults

    # Use config default agent if not specified
    if [[ -z "$agent" ]] && [[ -n "${NEEDLE_DEFAULT_AGENT:-}" ]]; then
        agent="$NEEDLE_DEFAULT_AGENT"
    fi

    # Use config default count if not specified
    if [[ -z "$count" ]] && [[ -n "${NEEDLE_DEFAULT_COUNT:-}" ]]; then
        count="$NEEDLE_DEFAULT_COUNT"
    fi

    # Validate agent (unless showing status)
    if [[ "$show_status" != "true" ]]; then
        if ! _needle_validate_agent "$agent"; then
            exit $NEEDLE_EXIT_USAGE
        fi
    fi

    # Validate count
    if ! _needle_validate_count "$count"; then
        exit $NEEDLE_EXIT_USAGE
    fi

    # Validate budget
    if ! _needle_validate_budget "$budget"; then
        exit $NEEDLE_EXIT_USAGE
    fi

    return 0
}

# Export validated options as JSON for consumption by other tools
# Usage: _needle_export_validated_json
_needle_export_validated_json() {
    cat << EOF
{
    "workspace": "$(_needle_json_escape "${NEEDLE_VALIDATED_WORKSPACE:-}")",
    "agent": "$(_needle_json_escape "${NEEDLE_VALIDATED_AGENT:-}")",
    "count": ${NEEDLE_VALIDATED_COUNT:-1},
    "budget": "$(_needle_json_escape "${NEEDLE_VALIDATED_BUDGET:-}")",
    "no_hooks": ${NEEDLE_VALIDATED_NO_HOOKS:-false},
    "dry_run": ${NEEDLE_VALIDATED_DRY_RUN:-false},
    "force": ${NEEDLE_VALIDATED_FORCE:-false},
    "wait": ${NEEDLE_VALIDATED_WAIT:-false}
}
EOF
}

_needle_run() {
    # Parse and validate all arguments
    _needle_run_parse_args "$@"

    # Extract validated values (already exported)
    local workspace="$NEEDLE_VALIDATED_WORKSPACE"
    local agent="${NEEDLE_VALIDATED_AGENT:-}"
    local count="$NEEDLE_VALIDATED_COUNT"
    local budget="${NEEDLE_VALIDATED_BUDGET:-}"
    local session_name="${NEEDLE_VALIDATED_SESSION_NAME:-}"
    local no_hooks="$NEEDLE_VALIDATED_NO_HOOKS"
    local dry_run="$NEEDLE_VALIDATED_DRY_RUN"
    local force="$NEEDLE_VALIDATED_FORCE"
    local wait_for_slot="${NEEDLE_VALIDATED_WAIT:-false}"
    local show_status="$NEEDLE_VALIDATED_STATUS"

    # Handle --status flag
    if [[ "$show_status" == "true" ]]; then
        local status_agent="${agent:-$(_needle_get_default_agent 2>/dev/null)}"
        if [[ -z "$status_agent" ]]; then
            _needle_error "Agent required for status check. Use: needle run --agent=<name> --status"
            exit $NEEDLE_EXIT_USAGE
        fi

        # Load agent to get provider
        local provider=""
        if _needle_load_agent "$status_agent" &>/dev/null; then
            provider="${NEEDLE_AGENT[provider]:-}"
        fi

        _needle_show_concurrency_status "$status_agent" "$provider"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # At this point, agent should be validated and loaded
    local provider="${NEEDLE_AGENT[provider]:-}"

    # Check concurrency limits (unless --force or --dry-run)
    if [[ "$force" != "true" ]] && [[ "$dry_run" != "true" ]]; then
        _needle_verbose "Checking concurrency limits..."

        if ! _needle_check_concurrency "$agent" "$provider" "$count"; then
            # Handle --wait option
            if [[ "$wait_for_slot" == "true" ]]; then
                _needle_info "$NEEDLE_LIMIT_CHECK_MESSAGE"
                _needle_info "$NEEDLE_LIMIT_CHECK_DETAILS"
                _needle_print ""
                _needle_info "Waiting for a slot to become available..."

                # Poll until slot is available
                local wait_interval=5
                local max_wait=3600  # 1 hour max wait
                local waited=0

                while [[ $waited -lt $max_wait ]]; do
                    sleep $wait_interval
                    ((waited += wait_interval))

                    if _needle_check_concurrency "$agent" "$provider" "$count"; then
                        _needle_success "Slot available after ${waited}s"
                        break
                    fi

                    # Show progress every 30 seconds
                    if [[ $((waited % 30)) -eq 0 ]]; then
                        _needle_verbose "Still waiting... (${waited}s elapsed)"
                    fi
                done

                if [[ $waited -ge $max_wait ]]; then
                    _needle_error "Timeout waiting for slot after ${max_wait}s"
                    exit $NEEDLE_EXIT_TIMEOUT
                fi
            else
                _needle_error "$NEEDLE_LIMIT_CHECK_MESSAGE"
                _needle_info "$NEEDLE_LIMIT_CHECK_DETAILS"
                _needle_print ""
                _needle_info "Options:"
                _needle_info "  --wait     Wait for a slot to become available"
                _needle_info "  --force    Override limit (use with caution)"
                exit $NEEDLE_EXIT_USAGE
            fi
        fi

        _needle_success "Concurrency check passed"
        _needle_verbose "$NEEDLE_LIMIT_CHECK_DETAILS"
    fi

    # Display run configuration
    _needle_header "Running: $agent"

    if [[ "$dry_run" == "true" ]]; then
        _needle_info "Dry run mode - no changes will be made"
    fi

    _needle_verbose "Workspace: $workspace"
    _needle_verbose "Agent: $agent"
    _needle_verbose "Workers: $count"
    [[ -n "$budget" ]] && _needle_verbose "Budget: \$$budget"
    [[ "$no_hooks" == "true" ]] && _needle_verbose "Hooks: disabled"
    [[ "$wait_for_slot" == "true" ]] && _needle_verbose "Wait: enabled"
    _needle_verbose "Force: $force"

    # Start watchdog monitor (ensures it's running)
    # The watchdog monitors worker heartbeats and recovers stuck workers
    if [[ "$dry_run" != "true" ]]; then
        # Source watchdog monitor module
        if [[ -f "${NEEDLE_LIB_DIR:-}/watchdog/monitor.sh" ]]; then
            source "${NEEDLE_LIB_DIR:-}/watchdog/monitor.sh"
            _needle_ensure_watchdog
        else
            _needle_warn "Watchdog module not found, skipping watchdog startup"
        fi
    fi

    # Handle dry-run mode
    if [[ "$dry_run" == "true" ]]; then
        _needle_print ""
        _needle_section "Dry Run Summary"
        _needle_table_row "Workspace" "$workspace"
        _needle_table_row "Agent" "$agent"
        [[ -n "$provider" ]] && _needle_table_row "Provider" "$provider"
        _needle_table_row "Workers to start" "$count"
        [[ -n "$budget" ]] && _needle_table_row "Budget" "\$$budget"
        [[ "$no_hooks" == "true" ]] && _needle_table_row "Hooks" "disabled"

        if [[ -n "$agent" ]] && [[ -n "$provider" ]]; then
            _needle_print ""
            local status_json
            status_json=$(_needle_get_concurrency_status "$agent" "$provider")
            local global_current global_limit
            global_current=$(echo "$status_json" | jq -r '.global.current')
            global_limit=$(echo "$status_json" | jq -r '.global.limit')
            _needle_table_row "Current global workers" "$global_current / $global_limit"
        fi

        _needle_print ""
        _needle_section "Validated Options (JSON)"
        _needle_export_validated_json
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # Spawn workers
    local spawned_sessions=()
    local failed_count=0

    if [[ "$count" -eq 1 ]]; then
        # Single worker - use existing session creation
        local session
        session=$(_needle_spawn_single_worker "$workspace" "$agent" "$provider" "$budget" "$no_hooks" "$session_name")
        if [[ $? -eq 0 ]] && [[ -n "$session" ]]; then
            spawned_sessions+=("$session")
        else
            ((failed_count++))
        fi
    else
        # Multiple workers - spawn in parallel
        _needle_info "Spawning $count workers..."
        spawned_sessions=$(_needle_spawn_multiple_workers "$workspace" "$agent" "$provider" "$count" "$budget" "$no_hooks")
        failed_count=$((count - ${#spawned_sessions[@]}))
    fi

    # Report results
    _needle_print ""
    if [[ ${#spawned_sessions[@]} -gt 0 ]]; then
        _needle_success "Started ${#spawned_sessions[@]} worker(s)"
        _needle_print ""
        _needle_section "Worker Sessions"

        for session in "${spawned_sessions[@]}"; do
            _needle_table_row "Session" "$session"
            _needle_info "  Attach with: needle attach $session"
        done

        # Emit multi-worker start event
        if [[ ${#spawned_sessions[@]} -gt 1 ]]; then
            _needle_emit_event "workers.multi_spawned" \
                "Started multiple workers in parallel" \
                "count=${#spawned_sessions[@]}" \
                "agent=$agent" \
                "workspace=$workspace"
        fi
    else
        _needle_error "Failed to start any workers"
        exit $NEEDLE_EXIT_RUNTIME
    fi

    if [[ $failed_count -gt 0 ]]; then
        _needle_warn "$failed_count worker(s) failed to start"
    fi

    exit $NEEDLE_EXIT_SUCCESS
}

# -----------------------------------------------------------------------------
# Worker Spawning Functions
# -----------------------------------------------------------------------------

# Spawn a single worker in a tmux session
# Arguments:
#   $1 - Workspace path
#   $2 - Agent name
#   $3 - Provider name
#   $4 - Budget (optional)
#   $5 - No hooks flag (true/false)
#   $6 - Explicit session name (optional)
# Returns: Session name on success, empty on failure
# Usage: session=$(_needle_spawn_single_worker "/workspace" "claude-anthropic-sonnet" "anthropic" "10.00" "false" "my-session")
_needle_spawn_single_worker() {
    local workspace="$1"
    local agent="$2"
    local provider="$3"
    local budget="$4"
    local no_hooks="$5"
    local explicit_name="$6"

    # Parse agent name to get runner, provider, model components
    local runner model
    IFS='-' read -r runner _ model <<< "$agent"

    # Get next available identifier
    local identifier
    identifier=$(get_next_identifier "$agent")

    # Generate session name (use explicit name if provided)
    local session
    if [[ -n "$explicit_name" ]]; then
        session="$explicit_name"
    else
        session=$(_needle_generate_session_name "" "$runner" "$provider" "$model" "$identifier")
    fi

    # Build command to run worker
    local cmd_args=(
        "needle" "_run_worker"
        "--workspace" "$workspace"
        "--agent" "$agent"
        "--identifier" "$identifier"
        "--session" "$session"
    )

    [[ -n "$budget" ]] && cmd_args+=("--budget" "$budget")
    [[ "$no_hooks" == "true" ]] && cmd_args+=("--no-hooks")

    # Create tmux session
    if _needle_create_session "$session" "${cmd_args[*]}"; then
        echo "$session"
        return 0
    else
        return 1
    fi
}

# Spawn multiple workers in parallel
# Arguments:
#   $1 - Workspace path
#   $2 - Agent name
#   $3 - Provider name
#   $4 - Number of workers to spawn
#   $5 - Budget (optional)
#   $6 - No hooks flag (true/false)
# Returns: Array of session names (newline-separated)
# Usage: sessions=$(_needle_spawn_multiple_workers "/workspace" "claude-anthropic-sonnet" "anthropic" 5 "10.00" "false")
_needle_spawn_multiple_workers() {
    local workspace="$1"
    local agent="$2"
    local provider="$3"
    local count="$4"
    local budget="$5"
    local no_hooks="$6"

    # Parse agent name to get runner, provider, model components
    local runner model
    IFS='-' read -r runner _ model <<< "$agent"

    # Get list of existing identifiers for this agent
    local existing_identifiers
    existing_identifiers=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | \
        grep "^needle-$agent-" | sed 's/.*-//' | tr '\n' ' ' || echo "")

    # Allocate unique identifiers for all workers
    local identifiers=()
    local used="$existing_identifiers"

    for ((i = 0; i < count; i++)); do
        local next_id
        next_id=$(get_next_identifier_from_list "$used")
        identifiers+=("$next_id")
        used="$used $next_id"
    done

    # Spawn workers in parallel (non-blocking)
    local spawned_sessions=()
    local pids=()

    for identifier in "${identifiers[@]}"; do
        # Generate session name
        local session
        session=$(_needle_generate_session_name "" "$runner" "$provider" "$model" "$identifier")

        # Build command to run worker
        local cmd_args=(
            "needle" "_run_worker"
            "--workspace" "$workspace"
            "--agent" "$agent"
            "--identifier" "$identifier"
        )

        [[ -n "$budget" ]] && cmd_args+=("--budget" "$budget")
        [[ "$no_hooks" == "true" ]] && cmd_args+=("--no-hooks")

        # Create tmux session (non-blocking)
        if _needle_create_session "$session" "${cmd_args[*]}"; then
            spawned_sessions+=("$session")
            _needle_debug "Spawned worker: $session"
        else
            _needle_warn "Failed to spawn worker: $session"
        fi
    done

    # Return all spawned session names
    printf '%s\n' "${spawned_sessions[@]}"
}

# -----------------------------------------------------------------------------
# Module Integration Wrappers
# -----------------------------------------------------------------------------

# These wrapper functions ensure compatibility with naming.sh and tmux.sh modules.
# If the modules are loaded, use their functions; otherwise provide fallbacks.

# Get next available identifier for an agent
# Falls back to NATO alphabet from constants.sh if naming.sh not loaded
# Arguments:
#   $1 - Agent name (runner-provider-model)
# Returns: Next available NATO identifier
get_next_identifier() {
    if declare -f _needle_naming_get_next_identifier &>/dev/null; then
        _needle_naming_get_next_identifier "$1"
    else
        # Fallback: use naming.sh get_next_identifier if available
        if declare -f get_next_identifier &>/dev/null; then
            # Use the naming.sh function directly
            return 0
        fi

        # Inline implementation using NATO alphabet
        local agent="$1"
        local prefix="needle-$agent-"
        local existing
        existing=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep "^$prefix" | sed 's/.*-//' || true)

        for name in "${NEEDLE_NATO_ALPHABET[@]}"; do
            if ! echo "$existing" | grep -qx "$name"; then
                echo "$name"
                return 0
            fi
        done

        # All 26 NATO names used - add numeric suffix
        local count
        count=$(echo "$existing" | grep -c . 2>/dev/null || echo "0")
        echo "alpha-$((count + 1))"
    fi
}

# Get next available identifier from a list of already-used identifiers
# Arguments:
#   $1 - Space-separated list of used identifiers
# Returns: Next unused NATO identifier
get_next_identifier_from_list() {
    local used="$1"

    for name in "${NEEDLE_NATO_ALPHABET[@]}"; do
        if ! echo "$used" | grep -qw "$name"; then
            echo "$name"
            return 0
        fi
    done

    # All 26 NATO names used - add numeric suffix
    local count
    count=$(echo "$used" | wc -w | tr -d ' ')
    echo "alpha-$((count + 1))"
}

# Generate a session name from components
# Falls back to inline implementation if tmux.sh not loaded
# Arguments:
#   $1 - Pattern (optional, uses default)
#   $2 - Runner name
#   $3 - Provider name
#   $4 - Model name
#   $5 - Identifier
# Returns: Formatted session name
_needle_generate_session_name() {
    local pattern="${1:-}"
    local runner="${2:-unknown}"
    local provider="${3:-unknown}"
    local model="${4:-unknown}"
    local identifier="${5:-alpha}"

    if [[ -z "$pattern" ]]; then
        pattern='needle-{runner}-{provider}-{model}-{identifier}'
    fi

    local name="$pattern"
    name="${name//\{runner\}/$runner}"
    name="${name//\{provider\}/$provider}"
    name="${name//\{model\}/$model}"
    name="${name//\{identifier\}/$identifier}"

    # Sanitize for tmux
    name=$(echo "$name" | tr -cd '[:alnum:]._-')

    echo "$name"
}

# Create a tmux session
# Falls back to inline implementation if tmux.sh not loaded
# Arguments:
#   $1 - Session name
#   $2... - Command to run
# Returns: 0 on success, 1 on failure
_needle_create_session() {
    local session="$1"
    shift
    local cmd="$*"

    if [[ -z "$session" ]] || [[ -z "$cmd" ]]; then
        return 1
    fi

    # Check if tmux is available
    if ! command -v tmux &>/dev/null; then
        _needle_warn "tmux not available, cannot create session"
        return 1
    fi

    # Check if session already exists
    if tmux has-session -t "$session" 2>/dev/null; then
        _needle_warn "Session already exists: $session"
        return 1
    fi

    # Create detached session
    tmux new-session -d -s "$session" "$cmd"
    return $?
}

# -----------------------------------------------------------------------------
# Internal Worker Command (_run_worker)
# -----------------------------------------------------------------------------

# Internal command to run a worker inside a tmux session
# This is called by _needle_spawn_single_worker after creating the tmux session
# Arguments:
#   --workspace <PATH>    - Workspace directory
#   --agent <NAME>        - Agent name (e.g., claude-anthropic-sonnet)
#   --identifier <ID>     - Worker identifier (e.g., alpha)
#   --budget <USD>        - Budget override (optional)
#   --no-hooks            - Skip hook execution
#   --session <NAME>      - Session name (optional, auto-generated if not provided)
# Returns: Exits with worker exit code
# Usage: needle _run_worker --workspace /path --agent claude-anthropic-sonnet --identifier alpha
_needle_run_worker() {
    local workspace=""
    local agent=""
    local identifier=""
    local budget=""
    local no_hooks=false
    local session_name=""

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --workspace)
                workspace="$2"
                shift 2
                ;;
            --workspace=*)
                workspace="${1#*=}"
                shift
                ;;
            --agent)
                agent="$2"
                shift 2
                ;;
            --agent=*)
                agent="${1#*=}"
                shift
                ;;
            --identifier)
                identifier="$2"
                shift 2
                ;;
            --identifier=*)
                identifier="${1#*=}"
                shift
                ;;
            --budget)
                budget="$2"
                shift 2
                ;;
            --budget=*)
                budget="${1#*=}"
                shift
                ;;
            --no-hooks)
                no_hooks=true
                shift
                ;;
            --session)
                session_name="$2"
                shift 2
                ;;
            --session=*)
                session_name="${1#*=}"
                shift
                ;;
            -*)
                _needle_error "Unknown option: $1"
                exit $NEEDLE_EXIT_USAGE
                ;;
            *)
                _needle_error "Unexpected argument: $1"
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    # Validate required arguments
    if [[ -z "$workspace" ]]; then
        _needle_error "--workspace is required"
        exit $NEEDLE_EXIT_USAGE
    fi

    if [[ -z "$agent" ]]; then
        _needle_error "--agent is required"
        exit $NEEDLE_EXIT_USAGE
    fi

    if [[ -z "$identifier" ]]; then
        _needle_error "--identifier is required"
        exit $NEEDLE_EXIT_USAGE
    fi

    # Parse agent name to get components
    # Format: runner-provider-model (e.g., claude-anthropic-sonnet)
    local runner provider model
    IFS='-' read -r runner provider model <<< "$agent"

    if [[ -z "$runner" ]] || [[ -z "$provider" ]] || [[ -z "$model" ]]; then
        _needle_error "Invalid agent format: $agent (expected: runner-provider-model)"
        exit $NEEDLE_EXIT_USAGE
    fi

    # Generate session name if not provided
    if [[ -z "$session_name" ]]; then
        session_name=$(_needle_generate_session_name "" "$runner" "$provider" "$model" "$identifier")
    fi

    # Set up environment variables for the worker loop
    export NEEDLE_WORKSPACE="$workspace"
    export NEEDLE_AGENT="$agent"
    export NEEDLE_WORKER_ID="$identifier"
    export NEEDLE_SESSION="$session_name"
    export NEEDLE_RUNNER="$runner"
    export NEEDLE_PROVIDER="$provider"
    export NEEDLE_MODEL="$model"
    export NEEDLE_IDENTIFIER="$identifier"

    # Set optional environment variables
    if [[ -n "$budget" ]]; then
        export NEEDLE_BUDGET="$budget"
    fi

    if [[ "$no_hooks" == "true" ]]; then
        export NEEDLE_NO_HOOKS="true"
    fi

    # Set up state directory
    export NEEDLE_STATE_DIR="${NEEDLE_HOME:-$HOME/.needle}/state"
    export NEEDLE_LOG_DIR="${NEEDLE_HOME:-$HOME/.needle}/logs"

    # Ensure directories exist
    mkdir -p "$NEEDLE_STATE_DIR" "$NEEDLE_LOG_DIR"

    # Set up log file for this worker
    export NEEDLE_LOG_FILE="${NEEDLE_LOG_DIR}/${session_name}.log"

    # Load the agent configuration
    if ! _needle_load_agent "$agent"; then
        _needle_error "Failed to load agent configuration: $agent"
        exit $NEEDLE_EXIT_CONFIG
    fi

    # Source the worker loop module
    local loop_module="${NEEDLE_LIB_DIR:-}/runner/loop.sh"
    if [[ ! -f "$loop_module" ]]; then
        # Try relative path from script directory
        loop_module="${NEEDLE_ROOT_DIR:-}/src/runner/loop.sh"
    fi

    if [[ ! -f "$loop_module" ]]; then
        _needle_error "Worker loop module not found: $loop_module"
        exit $NEEDLE_EXIT_CONFIG
    fi

    source "$loop_module"

    # Log worker start event
    _needle_debug "Starting worker: session=$session_name agent=$agent workspace=$workspace"

    # Emit worker started event
    _needle_emit_event "worker.started" \
        "Worker started in tmux session" \
        "session=$session_name" \
        "agent=$agent" \
        "runner=$runner" \
        "provider=$provider" \
        "model=$model" \
        "identifier=$identifier" \
        "workspace=$workspace"

    # Enter the worker loop
    # This function runs indefinitely until the worker is stopped
    _needle_worker_loop "$workspace" "$agent"
}
