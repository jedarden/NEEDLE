#!/usr/bin/env bash
#
# NEEDLE Build Script
# Bundles all modules into a single distributable bash script
#
# Usage:
#   ./scripts/build.sh              # Build to dist/needle
#   ./scripts/build.sh --output FILE  # Build to custom path
#   ./scripts/build.sh --minify       # Strip comments (smaller file)
#
# Output:
#   dist/needle - Single self-contained bash script

set -euo pipefail

# -----------------------------------------------------------------------------
# Configuration
# -----------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
OUTPUT_FILE="${ROOT_DIR}/dist/needle"
MINIFY=false

# -----------------------------------------------------------------------------
# Parse Arguments
# -----------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case "$1" in
        --output|-o)
            OUTPUT_FILE="$2"
            shift 2
            ;;
        --minify|-m)
            MINIFY=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --output, -o FILE   Output file (default: dist/needle)"
            echo "  --minify, -m        Strip comments for smaller file"
            echo "  --help, -h          Show this help"
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

# -----------------------------------------------------------------------------
# Build Functions
# -----------------------------------------------------------------------------

# Get version from VERSION file
get_version() {
    cat "$ROOT_DIR/VERSION" | tr -d '\n'
}

# Strip source commands and module headers from a file
process_module() {
    local file="$1"
    local content

    content=$(cat "$file")

    # Remove shebang (we'll add one at the top)
    content=$(echo "$content" | sed '1{/^#!/d}')

    # Use perl to remove direct execution blocks, source commands, and module _LOADED assignments
    # Note: Source guards are already skipped by setting _LOADED vars at top
    content=$(echo "$content" | perl -0777 -pe '
        # Remove direct execution blocks (BASH_SOURCE checks) that test modules standalone
        # These cause module code to execute instead of just defining functions
        s/^if\s+\[\[\s+"\$\{BASH_SOURCE\[0\]\}"\s+==\s+"\$\{0\}"\s+\]\];\s+then\s*\n.*?^fi\s*$//gsm;

        # Remove ALL source commands (both guarded and unguarded)
        # Modules are inline, so source commands will fail
        s/^[ \t]*source\s+[^\n]+\n//gm;

        # Remove shellcheck source directives (left behind after source removal)
        s/^[ \t]*#\s*shellcheck\s+source=[^\n]*\n//gm;

        # Remove _LOADED=true assignments (we set these all at the top now)
        s/^[ \t]*[A-Z_]+_LOADED=true\s*\n//gm;

        # Clean up empty/comment-only if-blocks left after removing source commands
        # Run multiple times to handle nested empty blocks
        for my $i (1..5) {
            # Pattern: if ... then\n(only whitespace/comments)\nfi
            s/^[ \t]*if\s+[^\n]+;\s*then\s*\n([ \t]*#[^\n]*\n|[ \t]*\n)*\s*fi\s*\n//gm;

            # Pattern: if ... then\n(only whitespace/comments)\nelse\n...\nfi → just the else body
            s/^[ \t]*if\s+[^\n]+;\s*then\s*\n([ \t]*#[^\n]*\n|[ \t]*\n)*\s*else\s*\n(.*?)\n\s*fi\s*\n/$2\n/gsm;
        }
    ')

    if [[ "$MINIFY" == "true" ]]; then
        # Strip comment-only lines (keep inline comments)
        content=$(echo "$content" | grep -v '^[[:space:]]*#[^!]' || true)
        # Strip empty lines
        content=$(echo "$content" | grep -v '^[[:space:]]*$' || true)
    fi

    echo "$content"
}

# Embed YAML files as heredocs
embed_agents() {
    local agents_dir="$ROOT_DIR/config/agents"

    echo "# Embedded agent configurations"
    echo "# Extracted at runtime to \$NEEDLE_HOME/agents/ if not present"
    echo "declare -gA _NEEDLE_EMBEDDED_AGENTS"
    echo ""

    for yaml_file in "$agents_dir"/*.yaml; do
        if [[ -f "$yaml_file" ]]; then
            local name
            name=$(basename "$yaml_file" .yaml)
            echo "_NEEDLE_EMBEDDED_AGENTS[$name]=\$(cat <<'__AGENT_EOF__'"
            cat "$yaml_file"
            echo "__AGENT_EOF__"
            echo ")"
            echo ""
        fi
    done

    # Embed stream-parser.sh script (required by all claude agent YAML configs)
    local stream_parser="$agents_dir/stream-parser.sh"
    if [[ -f "$stream_parser" ]]; then
        echo "# Embedded stream-parser.sh script"
        echo "_NEEDLE_EMBEDDED_STREAM_PARSER=\$(cat <<'__STREAM_PARSER_EOF__'"
        cat "$stream_parser"
        echo "__STREAM_PARSER_EOF__"
        echo ")"
        echo ""
    fi
}

# Generate function to extract embedded agents
generate_agent_extractor() {
    cat <<'EXTRACTOR'
# Extract embedded agents to NEEDLE_HOME if not already present
_needle_extract_embedded_agents() {
    local agents_dir="${NEEDLE_HOME:-$HOME/.needle}/agents"
    mkdir -p "$agents_dir"

    for name in "${!_NEEDLE_EMBEDDED_AGENTS[@]}"; do
        local target="$agents_dir/${name}.yaml"
        if [[ ! -f "$target" ]]; then
            echo "${_NEEDLE_EMBEDDED_AGENTS[$name]}" > "$target"
        fi
    done

    # Always ensure stream-parser.sh is present and executable
    local stream_parser_target="$agents_dir/stream-parser.sh"
    if [[ ! -f "$stream_parser_target" ]] && [[ -n "${_NEEDLE_EMBEDDED_STREAM_PARSER:-}" ]]; then
        echo "$_NEEDLE_EMBEDDED_STREAM_PARSER" > "$stream_parser_target"
        chmod +x "$stream_parser_target"
    fi
}
EXTRACTOR
}

# -----------------------------------------------------------------------------
# Main Build
# -----------------------------------------------------------------------------

echo "Building NEEDLE $(get_version)..."

# Create output directory
mkdir -p "$(dirname "$OUTPUT_FILE")"

# Start with shebang and header
cat > "$OUTPUT_FILE" <<HEADER
#!/usr/bin/env bash
#
# NEEDLE - Navigates Every Enqueued Deliverable, Logs Effort
# Version: $(get_version)
#
# Universal orchestration wrapper for headless coding CLI agents
# https://github.com/jedarden/NEEDLE
#
# This is a bundled distribution. Source available at the URL above.
#
# Generated: $(date -u +"%Y-%m-%dT%H:%M:%SZ")
#

set -euo pipefail

# -----------------------------------------------------------------------------
# Version & Metadata
# -----------------------------------------------------------------------------

NEEDLE_VERSION="$(get_version)"
NEEDLE_BUILD_DATE="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

# -----------------------------------------------------------------------------
# Module Loading Guards
# -----------------------------------------------------------------------------
# Set all module _LOADED variables to prevent source attempts in bundled code
# This causes all source guards to skip, since modules are already inline
_NEEDLE_BILLING_MODELS_LOADED=true
_NEEDLE_CLAIM_LOADED=true
_NEEDLE_CONFIG_LOADED=true
_NEEDLE_CONSTANTS_LOADED=true
_NEEDLE_DIAGNOSTIC_LOADED=true
_NEEDLE_DISPATCH_LOADED=true
_NEEDLE_JSON_LOADED=true
_NEEDLE_MITOSIS_LOADED=true
_NEEDLE_OUTPUT_LOADED=true
_NEEDLE_PROMPT_LOADED=true
_NEEDLE_SELECT_LOADED=true
_NEEDLE_TELEMETRY_EVENTS_LOADED=true
_NEEDLE_WORKSPACE_LOADED=true

HEADER

# Add embedded agents
echo "" >> "$OUTPUT_FILE"
echo "# =============================================================================" >> "$OUTPUT_FILE"
echo "# EMBEDDED AGENT CONFIGURATIONS" >> "$OUTPUT_FILE"
echo "# =============================================================================" >> "$OUTPUT_FILE"
echo "" >> "$OUTPUT_FILE"
embed_agents >> "$OUTPUT_FILE"

# Add agent extractor function
echo "" >> "$OUTPUT_FILE"
generate_agent_extractor >> "$OUTPUT_FILE"

# Define module order (dependencies first)
MODULES=(
    # Core libraries (no dependencies)
    "src/lib/constants.sh"
    "src/lib/output.sh"
    "src/lib/json.sh"
    "src/lib/paths.sh"
    "src/lib/utils.sh"
    "src/lib/config.sh"
    "src/lib/config_schema.sh"
    "src/lib/workspace.sh"
    "src/lib/errors.sh"
    "src/lib/diagnostic.sh"
    "src/lib/billing_models.sh"
    "src/lib/update_check.sh"

    # Bootstrap
    "src/bootstrap/paths.sh"
    "bootstrap/check.sh"

    # Telemetry
    "src/telemetry/events.sh"
    "src/telemetry/fabric.sh"
    "src/telemetry/tokens.sh"
    "src/telemetry/budget.sh"
    "src/telemetry/effort.sh"
    "src/telemetry/writer.sh"

    # Agent system
    "src/agent/loader.sh"
    "src/agent/escape.sh"
    "src/agent/dispatch.sh"

    # Bead operations
    "src/bead/claim.sh"
    "src/bead/select.sh"
    "src/bead/prompt.sh"
    "src/bead/mitosis.sh"
    "src/bead/intent.sh"

    # Lock system
    "src/lock/checkout.sh"
    "src/lock/metrics.sh"

    # Quality
    "src/quality/bug_scanner.sh"

    # Watchdog
    "src/watchdog/heartbeat.sh"
    "src/watchdog/monitor.sh"

    # Hooks
    "src/hooks/runner.sh"
    "src/hooks/agent_settings.sh"
    "src/hooks/validate.sh"
    # NOTE: file-checkout.sh and post-execute-reconcile.sh are standalone hook
    # scripts (run as separate processes), not library modules. Do NOT bundle them.

    # Runner
    "src/runner/state.sh"
    "src/runner/naming.sh"
    "src/runner/tmux.sh"
    "src/runner/rate_limit.sh"
    "src/runner/limits.sh"
    "src/runner/loop.sh"

    # Strands
    "src/strands/pluck.sh"
    "src/strands/explore.sh"
    "src/strands/mend.sh"
    "src/strands/weave.sh"
    "src/strands/unravel.sh"
    "src/strands/pulse.sh"
    "src/strands/knot.sh"
    "src/strands/engine.sh"

    # Onboarding
    "src/onboarding/welcome.sh"
    "src/onboarding/agents.sh"
    "src/onboarding/create_config.sh"
    "src/onboarding/workspace_setup.sh"

    # CLI commands
    "src/cli/help.sh"
    "src/cli/init.sh"
    "src/cli/run.sh"
    "src/cli/list.sh"
    "src/cli/attach.sh"
    "src/cli/stop.sh"
    "src/cli/restart.sh"
    "src/cli/logs.sh"
    "src/cli/status.sh"
    "src/cli/version.sh"
    "src/cli/config.sh"
    "src/cli/heartbeat.sh"
    "src/cli/setup.sh"
    "src/cli/upgrade.sh"
    "src/cli/rollback.sh"
    "src/cli/pulse.sh"
    "src/cli/metrics.sh"
    "src/cli/analyze.sh"
    "src/cli/refactor.sh"
    "src/cli/test-agent.sh"
    "src/cli/agents.sh"
)

# Process each module
for module in "${MODULES[@]}"; do
    module_path="$ROOT_DIR/$module"

    if [[ ! -f "$module_path" ]]; then
        echo "Warning: Module not found: $module" >&2
        continue
    fi

    echo "" >> "$OUTPUT_FILE"
    echo "# =============================================================================" >> "$OUTPUT_FILE"
    echo "# MODULE: $module" >> "$OUTPUT_FILE"
    echo "# =============================================================================" >> "$OUTPUT_FILE"
    echo "" >> "$OUTPUT_FILE"

    process_module "$module_path" >> "$OUTPUT_FILE"
done

# Add main entry point
cat >> "$OUTPUT_FILE" <<'MAIN'

# =============================================================================
# MAIN ENTRY POINT
# =============================================================================

_needle_main() {
    # Extract embedded agents on first run
    _needle_extract_embedded_agents

    # Set NEEDLE_HOME if not set
    export NEEDLE_HOME="${NEEDLE_HOME:-$HOME/.needle}"

    # Parse global options
    local verbose=false
    local debug=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            -v|--verbose)
                verbose=true
                export NEEDLE_VERBOSE=true
                shift
                ;;
            -d|--debug)
                debug=true
                export NEEDLE_DEBUG=true
                shift
                ;;
            -h|--help)
                _needle_show_help
                return 0
                ;;
            -V|--version)
                _needle_show_version
                return 0
                ;;
            -*)
                # Unknown global flag - pass to subcommand
                break
                ;;
            *)
                # Subcommand
                break
                ;;
        esac
    done

    # Get subcommand
    local cmd="${1:-help}"
    shift || true

    # Dispatch to subcommand
    case "$cmd" in
        run|start)
            _needle_run "$@"
            ;;
        init)
            _needle_init "$@"
            ;;
        list|ls)
            _needle_list "$@"
            ;;
        attach)
            _needle_attach "$@"
            ;;
        stop)
            _needle_stop "$@"
            ;;
        restart)
            _needle_restart "$@"
            ;;
        logs)
            _needle_logs "$@"
            ;;
        status)
            _needle_status "$@"
            ;;
        config)
            _needle_config "$@"
            ;;
        heartbeat)
            _needle_heartbeat "$@"
            ;;
        setup)
            _needle_setup "$@"
            ;;
        upgrade)
            _needle_upgrade "$@"
            ;;
        rollback)
            _needle_rollback "$@"
            ;;
        pulse)
            _needle_pulse "$@"
            ;;
        metrics)
            _needle_metrics "$@"
            ;;
        analyze)
            _needle_analyze "$@"
            ;;
        refactor)
            _needle_refactor "$@"
            ;;
        agents|list-agents)
            _needle_agents "$@"
            ;;
        test-agent)
            _needle_test_agent "$@"
            ;;
        version|-V)
            _needle_show_version
            ;;
        _run_worker)
            _needle_run_worker "$@"
            ;;
        help|--help|-h)
            _needle_show_help
            ;;
        *)
            echo "Unknown command: $cmd" >&2
            echo "Run 'needle help' for usage information" >&2
            return 1
            ;;
    esac
}

_needle_show_help() {
    cat <<EOF
NEEDLE - Navigates Every Enqueued Deliverable, Logs Effort
Version: $NEEDLE_VERSION

Usage: needle [OPTIONS] <COMMAND> [ARGS]

Options:
  -v, --verbose     Enable verbose output
  -d, --debug       Enable debug output
  -h, --help        Show this help message
  -V, --version     Show version information

Commands:
  run               Start a NEEDLE worker
  list              List running workers
  attach            Attach to a worker's tmux session
  stop              Stop running worker(s)
  restart           Restart workers
  logs              View or tail worker logs
  status            Show worker health and statistics
  init              Initialize NEEDLE configuration
  setup             Check and install dependencies
  config            View or edit configuration
  heartbeat         Manage worker heartbeat and recovery
  agents            List available agent configurations
  test-agent        Test an agent configuration
  pulse             Run codebase health scan
  metrics           Analyze file collision effectiveness
  analyze           Analyze codebase patterns
  refactor          Suggest refactoring opportunities
  upgrade           Check for and install updates
  rollback          Rollback to a previous version
  version           Show version information
  help              Show this help message

Examples:
  needle run --workspace=/path --agent=claude-anthropic-sonnet
  needle list
  needle attach needle-claude-anthropic-sonnet-alpha
  needle stop --all

Documentation: https://github.com/jedarden/NEEDLE
EOF
}

_needle_show_version() {
    echo "NEEDLE $NEEDLE_VERSION"
    echo "Build: $NEEDLE_BUILD_DATE"
}

# Run main
_needle_main "$@"
MAIN

# Make executable
chmod +x "$OUTPUT_FILE"

# Report
FILE_SIZE=$(wc -c < "$OUTPUT_FILE" | tr -d ' ')
LINE_COUNT=$(wc -l < "$OUTPUT_FILE" | tr -d ' ')

echo ""
echo "Build complete!"
echo "  Output: $OUTPUT_FILE"
echo "  Size:   $FILE_SIZE bytes"
echo "  Lines:  $LINE_COUNT"
echo ""
echo "Test with: $OUTPUT_FILE --version"
