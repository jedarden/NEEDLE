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

        # Remove _LOADED=true assignments (we set these all at the top now)
        s/^[ \t]*[A-Z_]+_LOADED=true\s*\n//gm;

        # Clean up empty if-blocks that were left after removing source commands
        # Run multiple times to handle nested empty blocks
        for my $i (1..5) {
            # Pattern 1: if [[ ... ]]; then\nfi
            s/^[ \t]*if\s+\[\[.*?\]\]\s*;\s*then\s*\n\s*fi\s*\n//gm;
            # Pattern 2: if ! declare ...; then\nfi
            s/^[ \t]*if\s+!\s+declare.*?;\s*then\s*\n\s*fi\s*\n//gm;
            # Pattern 3: if COMMAND; then\nfi (general case)
            s/^[ \t]*if\s+[^;\n]+;\s*then\s*\n\s*fi\s*\n//gm;
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
}

# Generate function to extract embedded agents
generate_agent_extractor() {
    cat <<'EXTRACTOR'
# Extract embedded agents to NEEDLE_HOME if not already present
_needle_extract_embedded_agents() {
    local agents_dir="${NEEDLE_HOME:-$HOME/.needle}/agents"

    # Skip if agents directory already has files
    if [[ -d "$agents_dir" ]] && [[ -n "$(ls -A "$agents_dir" 2>/dev/null)" ]]; then
        return 0
    fi

    mkdir -p "$agents_dir"

    for name in "${!_NEEDLE_EMBEDDED_AGENTS[@]}"; do
        local target="$agents_dir/${name}.yaml"
        if [[ ! -f "$target" ]]; then
            echo "${_NEEDLE_EMBEDDED_AGENTS[$name]}" > "$target"
        fi
    done
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
# https://github.com/anthropics/needle
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
    "src/lib/utils.sh"
    "src/lib/config.sh"
    "src/lib/workspace.sh"

    # Bootstrap
    "src/bootstrap/paths.sh"

    # Telemetry
    "src/telemetry/events.sh"
    "src/telemetry/tokens.sh"
    "src/telemetry/budget.sh"
    "src/telemetry/effort.sh"

    # Agent system
    "src/agent/loader.sh"
    "src/agent/dispatch.sh"

    # Bead operations
    "src/bead/claim.sh"
    "src/bead/select.sh"
    "src/bead/prompt.sh"
    "src/bead/mitosis.sh"

    # Watchdog
    "src/watchdog/heartbeat.sh"

    # Hooks
    "src/hooks/runner.sh"

    # Runner
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

    # CLI commands
    "src/cli/init.sh"
    "src/cli/run.sh"
    "src/cli/version.sh"
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
            _needle_cmd_run "$@"
            ;;
        init|setup)
            _needle_cmd_init "$@"
            ;;
        version)
            _needle_show_version
            ;;
        agents|list-agents)
            _needle_agents "$@"
            ;;
        test-agent)
            _needle_cmd_test_agent "$@"
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
  init              Initialize NEEDLE configuration
  agents            List available agent configurations
  test-agent        Test an agent configuration
  version           Show version information
  help              Show this help message

Examples:
  needle init                           Initialize NEEDLE
  needle run --workspace=/path --agent=claude-anthropic-sonnet
  needle agents                         List available agents
  needle test-agent claude-anthropic-sonnet

Documentation: https://github.com/anthropics/needle
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
