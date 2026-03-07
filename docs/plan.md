# NEEDLE Implementation Plan

**N**avigates **E**very **E**nqueued **D**eliverable, **L**ogs **E**ffort

## Executive Summary

NEEDLE is a **universal wrapper** for headless coding CLI agents. It provides:

1. **Agent Abstraction** - Unified interface for Claude Code, OpenCode, Codex, and any headless CLI
2. **Task Navigation** - Priority-based bead queue processing across workspaces
3. **Effort Logging** - Time, tokens, and cost tracking per deliverable

**Core Design:**
- NEEDLE wraps any headless CLI that can execute a prompt and exit
- Multiple NEEDLE workers run in parallel without external orchestration
- Each worker selects beads, claims atomically, dispatches to the configured agent
- The agent executes and exits; control returns to NEEDLE for the next bead
- NEEDLE self-invokes into tmux for session management

**Supported Agents (via adapters):**
- **Claude Code** - Anthropic's CLI (`claude --print`)
- **OpenCode** - Open-source coding agent
- **Codex CLI** - OpenAI's coding CLI
- **Any headless CLI** - Generic adapter for custom agents

---

## Dependencies

NEEDLE integrates with and depends on the following external projects:

### Core Dependencies

| Dependency | Repository | Purpose |
|------------|------------|---------|
| **beads_rust** | [Dicklesworthstone/beads_rust](https://github.com/Dicklesworthstone/beads_rust) | Task queue and issue tracking |
| **ultimate_bug_scanner** | [Dicklesworthstone/ultimate_bug_scanner](https://github.com/Dicklesworthstone/ultimate_bug_scanner) | Code quality guardrails |

### beads_rust

Fast Rust port of Steve Yegge's beads: a local-first, non-invasive issue tracker storing tasks in SQLite with JSONL export for git collaboration.

**Why NEEDLE uses it:**
- Provides the bead queue that NEEDLE workers process
- SQLite storage enables atomic claims without external coordination
- JSONL export allows git-based collaboration across workspaces
- Local-first design aligns with NEEDLE's decentralized worker model

**Integration points:**
- `bead/claim.sh` - Atomic bead claiming via `br update --claim`
- `bead/select.sh` - Priority-weighted bead selection from queue
- `bead/mitosis.sh` - Bead decomposition analysis and child creation
- `strands/pluck.sh` - Reads OPEN beads from workspace `.beads/` directories

**Atomic Claim Workflow:**

The `br` CLI provides built-in atomic claiming via `br update --claim`:

```bash
# 1. Get claimable beads (unassigned, unblocked, not deferred)
br ready --unassigned --json

# 2. Attempt atomic claim (sets assignee + status in one transaction)
br update bd-xxx --claim --actor worker-alpha

# Success: exit 0, bead now assigned to worker-alpha with status=in_progress
# Race lost: exit 4, VALIDATION_FAILED "already assigned to worker-bravo"
```

**Race condition handling:** When multiple workers attempt to claim the same bead simultaneously, SQLite's transaction isolation ensures only one succeeds. The losing worker receives exit code 4 with an error message identifying who claimed it. NEEDLE's `bead/claim.sh` implements retry logic:

```bash
# bead/claim.sh pseudo-implementation
claim_bead() {
    local actor="$1"
    local max_retries=5

    for ((i=0; i<max_retries; i++)); do
        # Get claimable beads
        local candidates=$(br ready --unassigned --json)
        [[ -z "$candidates" || "$candidates" == "[]" ]] && return 1

        # Select one (priority-weighted)
        local bead_id=$(echo "$candidates" | select_weighted)

        # Attempt atomic claim
        if br update "$bead_id" --claim --actor "$actor" 2>/dev/null; then
            echo "$bead_id"
            return 0
        fi
        # Claim failed (race condition) - retry with different bead
    done
    return 1
}
```

**Key insight:** Atomicity comes from `br update --claim` (SQLite transactions), not file locks. The wrapper provides retry logic and weighted selection.

### ultimate_bug_scanner

Static analysis tool that catches 1000+ bug patterns across all popular programming languages, with auto-wiring into AI coding agent quality guardrails.

**Why NEEDLE uses it:**
- Quality gate before marking beads as completed
- Prevents agents from introducing common bug patterns
- Language-agnostic scanning works with any codebase
- Designed specifically for AI agent integration

**Integration points:**
- `runner/loop.sh` - Post-execution validation before bead completion
- `agent/dispatch.sh` - Optional pre-flight checks on generated code
- `effort/logger.sh` - Records scan results alongside execution metrics

### tmux

Terminal multiplexer that NEEDLE uses for worker session management.

**Why NEEDLE uses it:**
- Provides persistent sessions that survive SSH disconnects
- Enables parallel worker execution in isolated panes
- Allows attach/detach for monitoring and debugging
- Self-invoking design: `needle run` automatically creates tmux sessions

**Integration points:**
- `runner/tmux.sh` - Session creation, detection, and management
- `cli/run.sh` - Auto-creates `needle-{runner}-{provider}-{model}-{identifier}` sessions
- `cli/attach.sh` - Attaches to running worker sessions
- `cli/list.sh` - Lists active tmux sessions

### Headless Coding CLIs (Agents)

NEEDLE wraps headless coding CLIs to execute tasks. At least one must be installed:

| Agent CLI | Repository | Installation |
|-----------|------------|--------------|
| **Claude Code** | [anthropics/claude-code](https://github.com/anthropics/claude-code) | `npm install -g @anthropic-ai/claude-code` |
| **OpenCode** | [opencode-ai/opencode](https://github.com/opencode-ai/opencode) | `go install github.com/opencode-ai/opencode@latest` |
| **Codex CLI** | [openai/codex-cli](https://github.com/openai/codex-cli) | `npm install -g @openai/codex` |
| **Aider** | [paul-gauthier/aider](https://github.com/paul-gauthier/aider) | `pip install aider-chat` |

**Why NEEDLE uses them:**
- Provide the actual code generation and modification capabilities
- Each CLI executes prompts and exits (headless mode)
- NEEDLE is agent-agnostic: any CLI that can run a prompt and exit works
- Adapters normalize different input methods (stdin, file, args)

**Integration points:**
- `config/agents/*.yaml` - Per-agent configuration with invoke templates
- `agent/dispatch.sh` - Renders invoke template and executes via bash
- `agent/loader.sh` - Loads agent YAML configuration

### Runtime Dependencies

| Dependency | Version | Purpose | Auto-installed |
|------------|---------|---------|----------------|
| `bash` | 4.4+ | Shell execution | No (system) |
| `curl` | any | Downloading dependencies | No (system) |
| `tmux` | 3.0+ | Session management | **Yes** |
| `jq` | 1.6+ | JSON parsing | **Yes** |
| `yq` | 4.0+ | YAML parsing | **Yes** |
| `br` | latest | Bead queue management | **Yes** |

---

## Quick Start

### One-Liner Install

```bash
curl -fsSL https://needle.dev/install | bash
```

Or from GitHub releases:

```bash
curl -fsSL https://github.com/user/needle/releases/latest/download/install.sh | bash
```

The installer:
1. Downloads the latest `needle` binary to `~/.local/bin/`
2. Adds `~/.local/bin` to PATH if needed
3. Runs `needle init` to start interactive onboarding

### Manual Install

```bash
# Download specific version
curl -fsSL https://github.com/user/needle/releases/download/v1.0.0/needle -o ~/.local/bin/needle
chmod +x ~/.local/bin/needle

# Run onboarding
needle init
```

---

## Onboarding Experience

### Auto-Initialization

**Any `needle` command in an unconfigured environment automatically redirects to `needle init`.**

NEEDLE checks for configuration on every invocation:

```bash
# User runs any command without prior setup
$ needle run --workspace=/path/to/project

    No configuration found. Starting first-time setup...

    (automatically runs needle init)
```

**Detection logic:**
```bash
_needle_needs_init() {
  # No config file exists
  [[ ! -f "$HOME/.needle/config.yaml" ]] && return 0

  # Config exists but is invalid/empty
  [[ ! -s "$HOME/.needle/config.yaml" ]] && return 0

  # Dependencies not installed
  ! command -v tmux &>/dev/null && return 0
  ! command -v br &>/dev/null && return 0

  return 1
}

# At start of every command (except init, version, help)
if _needle_needs_init; then
  echo "No configuration found. Starting first-time setup..."
  exec needle init "$@"
fi
```

**Commands that skip auto-init:**
- `needle init` - Already the init command
- `needle version` - Should always work (shows "not configured" if needed)
- `needle help` - Should always show help
- `needle --help` / `needle -h` - Help flags

### First Run: `needle init`

When NEEDLE is first installed (or when running `needle init`), it provides an interactive onboarding experience:

```
$ needle init

    ╔═══════════════════════════════════════════════════════════════╗
    ║                                                               ║
    ║   ███╗   ██╗███████╗███████╗██████╗ ██╗     ███████╗         ║
    ║   ████╗  ██║██╔════╝██╔════╝██╔══██╗██║     ██╔════╝         ║
    ║   ██╔██╗ ██║█████╗  █████╗  ██║  ██║██║     █████╗           ║
    ║   ██║╚██╗██║██╔══╝  ██╔══╝  ██║  ██║██║     ██╔══╝           ║
    ║   ██║ ╚████║███████╗███████╗██████╔╝███████╗███████╗         ║
    ║   ╚═╝  ╚═══╝╚══════╝╚══════╝╚═════╝ ╚══════╝╚══════╝         ║
    ║                                                               ║
    ║   Navigates Every Enqueued Deliverable, Logs Effort          ║
    ║                                                               ║
    ╚═══════════════════════════════════════════════════════════════╝

    Welcome to NEEDLE! Let's get you set up.

Step 1/4: Installing dependencies...

    [✓] tmux 3.4 (already installed)
    [✓] jq 1.7 (already installed)
    [↓] yq 4.40 (installing from GitHub...)
    [↓] br 0.8.0 (installing from GitHub...)

    All dependencies installed!

Step 2/4: Detecting available agents...

    Scanning PATH for coding CLIs...

    [✓] claude (Claude Code v1.0.30)
        └─ Auth: logged in as user@example.com
    [✓] opencode (OpenCode v0.5.0)
        └─ Auth: API key configured
    [✗] codex (not found)
        └─ Install: npm install -g @openai/codex
    [✗] aider (not found)
        └─ Install: pip install aider-chat

    2 agents ready, 2 not installed (optional)

Step 3/4: Configure your first workspace...

    Enter workspace path (or press Enter for current directory):
    > /home/coder/my-project

    Checking for beads workspace...
    [✓] Found .beads/ with 12 open issues

    Which agent would you like to use? (default: claude-anthropic-sonnet)
    > claude-anthropic-sonnet

Step 4/4: Create default configuration...

    Creating ~/.needle/config.yaml with sensible defaults...
    [✓] Configuration created

    ┌─────────────────────────────────────────────────────────────┐
    │                     Setup Complete!                         │
    ├─────────────────────────────────────────────────────────────┤
    │                                                             │
    │  Start your first worker:                                   │
    │                                                             │
    │    needle run                                               │
    │                                                             │
    │  Or with explicit options:                                  │
    │                                                             │
    │    needle run --workspace=/home/coder/my-project \          │
    │               --agent=claude-anthropic-sonnet               │
    │                                                             │
    │  Useful commands:                                           │
    │    needle list          # Show running workers              │
    │    needle attach alpha  # Attach to worker session          │
    │    needle stop --all    # Stop all workers                  │
    │    needle help          # Full documentation                │
    │                                                             │
    └─────────────────────────────────────────────────────────────┘
```

### Onboarding Steps

| Step | What it does |
|------|--------------|
| **1. Dependencies** | Installs tmux, jq, yq, br (auto-detects OS/package manager) |
| **2. Agent Detection** | Scans PATH for supported coding CLIs, checks auth status |
| **3. Workspace Setup** | Prompts for workspace, validates beads presence |
| **4. Configuration** | Creates `~/.needle/config.yaml` with detected settings |

### Non-Interactive Mode

For CI/CD or scripted installs:

```bash
# Install without prompts (uses defaults)
curl -fsSL https://needle.dev/install | bash -s -- --non-interactive

# Or after manual install
needle init --non-interactive --workspace=/path/to/project --agent=claude-anthropic-sonnet
```

### Re-running Onboarding

```bash
# Re-run full onboarding (won't overwrite existing config unless --force)
needle init

# Reset and re-run (overwrites config)
needle init --force

# Just re-check dependencies
needle setup

# Just re-detect agents
needle agents --scan
```

---

## NEEDLE Binary & Bootstrap

NEEDLE is distributed as a **single self-contained bash script** that bootstraps its own dependencies on first run.

### Dependency Installation Methods

| Dependency | Linux (apt/dnf/pacman) | macOS (brew) | Manual fallback |
|------------|------------------------|--------------|-----------------|
| `tmux` | `apt install tmux` | `brew install tmux` | Build from source |
| `jq` | `apt install jq` | `brew install jq` | Download binary |
| `yq` | Download binary | `brew install yq` | Download binary |
| `br` | Download from GitHub releases | Download from GitHub | Cargo install |

### Bootstrap Location

Dependencies are installed to `~/.local/bin` (added to PATH if needed). NEEDLE stores its configuration and state in:

```
~/.needle/
├── config.yaml              # User configuration (created from defaults)
├── agents/                  # Custom agent adapters (user-defined)
│   └── my-custom-agent.yaml
├── logs/                    # Structured event logs (JSONL per session)
│   ├── needle-claude-anthropic-sonnet-alpha.jsonl
│   ├── needle-claude-anthropic-sonnet-bravo.jsonl
│   └── needle-opencode-alibaba-qwen-charlie.jsonl
├── state/                   # Runtime state
│   ├── workers.json         # Active worker registry
│   ├── rate_limits/         # Rate limit tracking per provider
│   ├── heartbeats/          # Worker heartbeat files (stuck detection)
│   │   ├── needle-...-alpha.json
│   │   └── needle-...-bravo.json
│   └── pulse/               # Pulse strand state (codebase health)
│       ├── last_scan.json   # Timestamp and results of last scan
│       └── seen_issues.json # Issues already converted to beads (dedup)
├── hooks/                   # User-defined lifecycle hooks
│   ├── pre-claim.sh
│   ├── post-execute.sh
│   └── pre-complete.sh
└── cache/                   # Downloaded binaries and update artifacts
    ├── br                   # Cached dependency binaries
    ├── jq
    ├── yq
    ├── version_check        # Cached latest version (24h TTL)
    ├── needle-1.2.0.bak     # Backup of previous version (for rollback)
    └── needle-1.3.0.new     # Downloaded update (before swap)
```

### Self-Update System

NEEDLE includes automatic version checking and seamless binary updates.

#### Version Check on Startup

Every time NEEDLE starts, it performs a **non-blocking** version check:

```
$ needle run --workspace=/path

    NEEDLE v1.2.0 → v1.3.0 available
    Run 'needle upgrade' to update (changelog: https://github.com/user/needle/releases/tag/v1.3.0)

Starting worker alpha...
```

The check:
1. Fetches `https://api.github.com/repos/user/needle/releases/latest` (cached for 24h)
2. Compares remote version with local `NEEDLE_VERSION`
3. Displays non-intrusive notification if update available
4. **Never blocks** - workers start immediately regardless of update status

#### Automatic Update Command

```bash
# Check for updates
needle upgrade --check

# Download and install update
needle upgrade

# Force reinstall current version
needle upgrade --force

# Install specific version
needle upgrade --version=1.2.0

# Skip confirmation prompt
needle upgrade --yes
```

#### Seamless Binary Swap

The upgrade process ensures zero downtime and safe rollback:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                       NEEDLE UPGRADE FLOW                                   │
│                                                                             │
│   $ needle upgrade                                                          │
│                                                                             │
│   1. PREFLIGHT                                                              │
│      ├─ Check GitHub releases API for latest version                       │
│      ├─ Compare checksums to verify download needed                         │
│      └─ Verify write permissions to ~/.local/bin/                           │
│                                                                             │
│   2. DOWNLOAD (atomic)                                                      │
│      ├─ Download new binary to ~/.needle/cache/needle-{version}.new        │
│      ├─ Verify SHA256 checksum against release                              │
│      └─ Verify binary is executable (./needle-new --version)                │
│                                                                             │
│   3. BACKUP                                                                 │
│      └─ Move current binary to ~/.needle/cache/needle-{old-version}.bak    │
│                                                                             │
│   4. SWAP (atomic)                                                          │
│      └─ mv ~/.needle/cache/needle-{version}.new ~/.local/bin/needle        │
│                                                                             │
│   5. VERIFY                                                                 │
│      ├─ Run 'needle --version' to confirm upgrade                           │
│      ├─ On success: delete backup                                           │
│      └─ On failure: restore backup, report error                            │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

#### Upgrade Output

```
$ needle upgrade

    Checking for updates...

    Current version: v1.2.0
    Latest version:  v1.3.0

    Changelog highlights:
      • Added bead mitosis for automatic task decomposition
      • Fixed rate limit handling for Claude API
      • Improved telemetry for strand transitions

    Full changelog: https://github.com/user/needle/releases/tag/v1.3.0

    Proceed with upgrade? [Y/n] y

    [↓] Downloading needle v1.3.0...
    [✓] Downloaded (2.1 MB)
    [✓] Checksum verified (sha256: a1b2c3...)
    [✓] Binary validated
    [→] Backing up current version...
    [→] Installing new version...
    [✓] Upgrade complete!

    NEEDLE v1.3.0 is now installed.

    Note: Running workers will continue using v1.2.0 until restarted.
    Run 'needle restart --all' to restart workers with new version.
```

#### Running Workers During Upgrade

Since NEEDLE workers run in tmux sessions:
- **Active workers continue running** with the old binary (already in memory)
- The upgrade **only affects new worker launches**
- Users can restart workers at their convenience:

```bash
# Graceful restart: wait for current bead to complete, then restart
needle restart --all --graceful

# Immediate restart: interrupt current work (bead returned to queue)
needle restart --all --immediate

# Restart specific worker
needle restart alpha
```

#### Rollback

If an upgrade causes issues:

```bash
# List available versions (including backups)
needle upgrade --list

# Rollback to previous version
needle rollback

# Rollback to specific version
needle rollback --version=1.2.0
```

#### Offline / Air-gapped Environments

For environments without internet access:

```bash
# On connected machine: download binary
curl -fsSL https://github.com/user/needle/releases/download/v1.3.0/needle -o needle-1.3.0

# Transfer to air-gapped machine, then:
needle upgrade --local=/path/to/needle-1.3.0
```

#### Update Configuration

```yaml
# ~/.needle/config.yaml
updates:
  # Check for updates on startup (default: true)
  check_on_startup: true

  # How often to check (even if needle runs multiple times)
  check_interval: 24h

  # Auto-install updates without prompting (default: false)
  auto_upgrade: false

  # Include pre-release versions (default: false)
  include_prereleases: false

  # Disable update checks entirely (for air-gapped environments)
  disabled: false

  # Custom release URL (for self-hosted/enterprise)
  release_url: "https://github.com/user/needle/releases"
```

#### Implementation Details

```bash
# Version check (non-blocking, cached)
_needle_check_update() {
  local cache_file="$HOME/.needle/cache/version_check"
  local cache_ttl=86400  # 24 hours

  # Skip if recently checked
  if [[ -f "$cache_file" ]] && [[ $(($(date +%s) - $(stat -c %Y "$cache_file"))) -lt $cache_ttl ]]; then
    cat "$cache_file"
    return
  fi

  # Background fetch (non-blocking)
  (
    latest=$(curl -sf "https://api.github.com/repos/user/needle/releases/latest" | jq -r '.tag_name')
    echo "$latest" > "$cache_file"
  ) &

  # Return cached value or empty
  [[ -f "$cache_file" ]] && cat "$cache_file"
}

# Atomic binary swap
_needle_swap_binary() {
  local new_binary="$1"
  local target="$HOME/.local/bin/needle"
  local backup="$HOME/.needle/cache/needle-$(needle --version).bak"

  # Verify new binary works
  if ! "$new_binary" --version &>/dev/null; then
    echo "Error: New binary failed validation"
    return 1
  fi

  # Atomic swap with backup
  mv "$target" "$backup" && mv "$new_binary" "$target"

  # Verify swap succeeded
  if ! needle --version &>/dev/null; then
    echo "Error: Upgrade failed, restoring backup..."
    mv "$backup" "$target"
    return 1
  fi

  echo "Upgrade successful"
}
```

### CLI Commands

```bash
# Onboarding (auto-invoked if no config exists)
needle init              # Interactive first-time setup
needle init --force      # Re-run onboarding, overwrite config
needle init --check      # Check if initialization needed (exit 0=yes, 1=no)

# Bootstrap & Setup
needle setup             # Check and install dependencies
needle setup --check     # Check dependencies without installing
needle setup --reinstall # Force reinstall all dependencies
needle version           # Show needle and dependency versions

# Updates
needle upgrade           # Download and install latest version
needle upgrade --check   # Check for updates without installing
needle upgrade --yes     # Upgrade without confirmation prompt
needle rollback          # Rollback to previous version

# Agent Management
needle agents            # List configured agents
needle agents --scan     # Re-scan PATH for available agents
needle test-agent <name> # Test an agent adapter
```

### CLI Help System

Every command and subcommand provides detailed `--help` output.

#### Top-Level Help: `needle --help`

```
$ needle --help

NEEDLE - Navigates Every Enqueued Deliverable, Logs Effort

A universal wrapper for headless coding CLI agents that processes
beads (tasks) from a queue with automatic session management.

USAGE:
    needle <COMMAND> [OPTIONS]
    needle [OPTIONS]

COMMANDS:
    init        Interactive first-time setup and onboarding
    run         Start a worker to process beads
    list        List running workers
    attach      Attach to a worker's tmux session
    stop        Stop running worker(s)
    logs        View or tail worker logs
    status      Show worker health and statistics

    setup       Check and install dependencies
    upgrade     Check for and install updates
    rollback    Rollback to a previous version
    version     Show version information

    agents      List and manage agent adapters
    test-agent  Test an agent adapter configuration

    heartbeat   Manage worker heartbeat and recovery
    config      View or edit configuration

OPTIONS:
    -h, --help       Print help information
    -V, --version    Print version information
    -v, --verbose    Enable verbose output
    -q, --quiet      Suppress non-error output
    --no-color       Disable colored output

QUICK START:
    # First time setup (runs automatically if unconfigured)
    needle init

    # Start a worker
    needle run --workspace=/path/to/project --agent=claude-anthropic-sonnet

    # List running workers
    needle list

    # Attach to a worker session
    needle attach alpha

CONFIGURATION:
    Global config:    ~/.needle/config.yaml
    Workspace config: .needle.yaml (in workspace root)
    Logs:             ~/.needle/logs/

DOCUMENTATION:
    Full docs:  https://github.com/user/needle#readme
    Issues:     https://github.com/user/needle/issues
```

#### `needle init --help`

```
$ needle init --help

Initialize NEEDLE with interactive onboarding

This command guides you through first-time setup:
  1. Installing dependencies (tmux, jq, yq, br)
  2. Detecting available coding CLI agents
  3. Configuring your first workspace
  4. Creating default configuration

USAGE:
    needle init [OPTIONS]

OPTIONS:
    -f, --force              Overwrite existing configuration
    -c, --check              Check if initialization is needed (exit 0=yes, 1=no)
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
```

#### `needle run --help`

```
$ needle run --help

Start a worker to process beads from the queue

Launches a NEEDLE worker that claims and executes beads. The worker
runs in a tmux session for persistence and can be attached/detached.

USAGE:
    needle run [OPTIONS]

OPTIONS:
    -w, --workspace <PATH>   Workspace directory to process beads from
                             [default: current directory or config default]

    -a, --agent <NAME>       Agent adapter to use for execution
                             [default: from config]

    -i, --id <ID>            Worker identifier (e.g., "alpha", "primary")
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
```

#### `needle list --help`

```
$ needle list --help

List running NEEDLE workers

Shows all active workers with their current status, bead being
processed, and runtime statistics.

USAGE:
    needle list [OPTIONS]

OPTIONS:
    -a, --all                Include stopped/crashed workers
    -j, --json               Output as JSON
    -w, --wide               Show extended information

    --runner <NAME>          Filter by runner (e.g., "claude")
    --provider <NAME>        Filter by provider (e.g., "anthropic")
    --model <NAME>           Filter by model (e.g., "sonnet")
    --workspace <PATH>       Filter by workspace

    -h, --help               Print help information

OUTPUT COLUMNS:
    WORKER      Session name (e.g., needle-claude-anthropic-sonnet-alpha)
    STATUS      running | idle | executing | draining | stuck
    BEAD        Current bead ID or "(idle)"
    DURATION    Time on current bead or idle time
    WORKSPACE   Workspace path
    BEADS       Total beads completed this session

EXAMPLES:
    # List all running workers
    needle list

    # Show extended info
    needle list --wide

    # Filter by provider
    needle list --provider=anthropic

    # JSON output for scripting
    needle list --json | jq '.[] | select(.status == "executing")'

    # Include crashed workers
    needle list --all
```

#### `needle attach --help`

```
$ needle attach --help

Attach to a worker's tmux session

Connects your terminal to a running worker's tmux session for
monitoring or debugging. Detach with Ctrl+B, D.

USAGE:
    needle attach [WORKER]

ARGUMENTS:
    [WORKER]    Worker identifier or full session name
                Can be: "alpha", "bravo", or full name like
                "needle-claude-anthropic-sonnet-alpha"
                [default: most recently started worker]

OPTIONS:
    -r, --read-only          Attach in read-only mode (view only)
    -l, --last               Attach to most recent worker

    -h, --help               Print help information

EXAMPLES:
    # Attach to worker alpha
    needle attach alpha

    # Attach to most recent worker
    needle attach --last

    # Attach read-only (can't type, just watch)
    needle attach alpha --read-only

    # Attach by full session name
    needle attach needle-claude-anthropic-sonnet-alpha

DETACHING:
    Press Ctrl+B, then D to detach from the session.
    The worker continues running in the background.

SEE ALSO:
    needle list    List available workers to attach to
    needle logs    View worker logs without attaching
```

#### `needle stop --help`

```
$ needle stop --help

Stop running NEEDLE worker(s)

Stops one or more workers. By default, uses graceful shutdown which
waits for the current bead to complete before exiting.

USAGE:
    needle stop [WORKERS...] [OPTIONS]

ARGUMENTS:
    [WORKERS...]    Worker identifiers to stop (e.g., "alpha bravo")
                    Use --all to stop all workers

OPTIONS:
    -a, --all                Stop all running workers
    -g, --graceful           Wait for current bead to complete [default]
    -i, --immediate          Stop immediately, release current bead
    -f, --force              Kill process without cleanup (last resort)

    --timeout <SECONDS>      Graceful shutdown timeout [default: 300]

    -h, --help               Print help information

SHUTDOWN MODES:
    graceful    Finish current bead, then exit cleanly
    immediate   Release current bead to queue, exit now
    force       SIGKILL the process (may orphan bead)

EXAMPLES:
    # Stop worker alpha gracefully
    needle stop alpha

    # Stop all workers gracefully
    needle stop --all

    # Stop immediately (release bead back to queue)
    needle stop alpha --immediate

    # Stop all with custom timeout
    needle stop --all --timeout=60

    # Force kill (use only if stuck)
    needle stop alpha --force

NOTES:
    - Graceful shutdown waits for current bead to complete
    - If timeout is reached, bead is released and worker exits
    - Force stop may leave beads in inconsistent state
```

#### `needle logs --help`

```
$ needle logs --help

View or tail worker logs

Displays structured JSONL logs from worker sessions. Can filter
by event type, time range, or bead.

USAGE:
    needle logs [WORKER] [OPTIONS]

ARGUMENTS:
    [WORKER]    Worker identifier [default: all workers]

OPTIONS:
    -f, --follow             Follow log output (like tail -f)
    -n, --lines <N>          Number of lines to show [default: 50]

    --since <TIME>           Show logs since timestamp or duration
                             (e.g., "2024-01-01", "1h", "30m")
    --until <TIME>           Show logs until timestamp

    --event <TYPE>           Filter by event type (e.g., "bead.completed")
    --bead <ID>              Filter by bead ID
    --strand <N>             Filter by strand number (1-7)

    --raw                    Show raw JSONL without formatting
    -j, --json               Output as JSON array

    -h, --help               Print help information

EVENT TYPES:
    worker.*      Worker lifecycle (started, stopped, idle)
    bead.*        Bead processing (claimed, completed, failed)
    strand.*      Strand transitions (started, fallthrough)
    hook.*        Hook execution (started, completed, failed)
    heartbeat.*   Heartbeat events
    error.*       Error events

EXAMPLES:
    # View recent logs for all workers
    needle logs

    # Follow logs for worker alpha
    needle logs alpha --follow

    # Show last 100 lines
    needle logs --lines=100

    # Filter by event type
    needle logs --event=bead.completed

    # Show logs for specific bead
    needle logs --bead=bd-123

    # Logs from last hour
    needle logs --since=1h

    # Raw JSONL output for parsing
    needle logs --raw | jq 'select(.event == "bead.failed")'
```

#### `needle status --help`

```
$ needle status --help

Show worker health and statistics

Displays a dashboard of worker status, bead statistics, and
system health metrics.

USAGE:
    needle status [OPTIONS]

OPTIONS:
    -w, --watch              Refresh continuously (every 2s)
    -j, --json               Output as JSON

    -h, --help               Print help information

DASHBOARD SECTIONS:
    Workers     Running/idle/stuck workers with current beads
    Beads       Completed/failed/in-progress counts
    Strands     Activity by strand
    Effort      Token usage and cost estimates
    Health      Heartbeat status, quarantined beads

EXAMPLES:
    # Show status dashboard
    needle status

    # Watch continuously
    needle status --watch

    # JSON output for monitoring
    needle status --json
```

#### `needle setup --help`

```
$ needle setup --help

Check and install NEEDLE dependencies

Verifies that required dependencies are installed and optionally
installs missing ones.

USAGE:
    needle setup [OPTIONS]

OPTIONS:
    -c, --check              Check only, don't install anything
    -r, --reinstall          Force reinstall all dependencies
    -y, --yes                Don't prompt for confirmation

    -h, --help               Print help information

DEPENDENCIES:
    tmux     Terminal multiplexer for session management
    jq       JSON processor for parsing agent output
    yq       YAML processor for configuration
    br       Beads CLI for task queue management

EXAMPLES:
    # Check and install missing dependencies
    needle setup

    # Check only (for CI/CD)
    needle setup --check

    # Force reinstall everything
    needle setup --reinstall --yes

EXIT CODES:
    0    All dependencies installed/available
    1    Missing dependencies (with --check)
    2    Installation failed
```

#### `needle upgrade --help`

```
$ needle upgrade --help

Check for and install NEEDLE updates

Downloads and installs the latest version of NEEDLE from GitHub
releases with automatic backup and rollback support.

USAGE:
    needle upgrade [OPTIONS]

OPTIONS:
    -c, --check              Check for updates without installing
    -y, --yes                Upgrade without confirmation
    -f, --force              Reinstall even if already latest
    -l, --list               List available versions

    --version <VERSION>      Install specific version (e.g., "1.2.0")
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
```

#### `needle rollback --help`

```
$ needle rollback --help

Rollback to a previous NEEDLE version

Restores a previously installed version from the backup cache.

USAGE:
    needle rollback [OPTIONS]

OPTIONS:
    --version <VERSION>      Rollback to specific version
    -l, --list               List available versions for rollback
    -y, --yes                Rollback without confirmation

    -h, --help               Print help information

EXAMPLES:
    # Rollback to previous version
    needle rollback

    # Rollback to specific version
    needle rollback --version=1.1.0

    # List available backups
    needle rollback --list
```

#### `needle agents --help`

```
$ needle agents --help

List and manage agent adapters

Shows configured agent adapters and their availability status.
Can scan for new agents or test specific adapters.

USAGE:
    needle agents [OPTIONS]

OPTIONS:
    -s, --scan               Re-scan PATH for available agents
    -j, --json               Output as JSON
    -a, --all                Include unavailable agents

    -h, --help               Print help information

OUTPUT:
    NAME         Agent adapter name (e.g., claude-anthropic-sonnet)
    RUNNER       CLI executable (e.g., claude)
    STATUS       available | missing | auth-required
    VERSION      Detected version if available

EXAMPLES:
    # List configured agents
    needle agents

    # Scan for new agents
    needle agents --scan

    # JSON output
    needle agents --json

SEE ALSO:
    needle test-agent    Test a specific agent adapter
```

#### `needle test-agent --help`

```
$ needle test-agent --help

Test an agent adapter configuration

Validates an agent adapter by running a simple test prompt and
checking the output format.

USAGE:
    needle test-agent <AGENT> [OPTIONS]

ARGUMENTS:
    <AGENT>     Agent adapter name (e.g., claude-anthropic-sonnet)

OPTIONS:
    -p, --prompt <TEXT>      Custom test prompt [default: "echo hello"]
    -t, --timeout <SECS>     Timeout for test [default: 60]
    -v, --verbose            Show full agent output

    -h, --help               Print help information

TESTS PERFORMED:
    1. Agent CLI exists in PATH
    2. Invoke template renders correctly
    3. Agent executes and exits with code 0
    4. Output is parseable

EXAMPLES:
    # Test an agent
    needle test-agent claude-anthropic-sonnet

    # Test with custom prompt
    needle test-agent opencode-alibaba-qwen --prompt="print hello world"

    # Verbose output for debugging
    needle test-agent claude-anthropic-sonnet --verbose

EXIT CODES:
    0    Agent working correctly
    1    Agent CLI not found
    2    Execution failed
    3    Output parsing failed
```

#### `needle heartbeat --help`

```
$ needle heartbeat --help

Manage worker heartbeat and auto-recovery

Monitor worker health via heartbeats and manage the automatic
recovery watchdog.

USAGE:
    needle heartbeat <COMMAND> [OPTIONS]

COMMANDS:
    status      Show heartbeat status of all workers
    recover     Manually trigger recovery for stuck worker
    pause       Pause automatic recovery
    resume      Resume automatic recovery

OPTIONS:
    -h, --help    Print help information

EXAMPLES:
    # Check heartbeat status
    needle heartbeat status

    # Manually recover stuck worker
    needle heartbeat recover alpha

    # Pause auto-recovery (maintenance)
    needle heartbeat pause

    # Resume auto-recovery
    needle heartbeat resume
```

#### `needle heartbeat status --help`

```
$ needle heartbeat status --help

Show heartbeat status of all workers

Displays the last heartbeat time and health status for all
registered workers.

USAGE:
    needle heartbeat status [OPTIONS]

OPTIONS:
    -j, --json     Output as JSON
    -h, --help     Print help information

OUTPUT COLUMNS:
    WORKER        Worker session name
    STATUS        healthy | warning | STUCK | dead
    LAST BEAT     Time since last heartbeat
    CURRENT BEAD  Bead being processed or "(idle)"
    BEAD TIME     Time on current bead

EXAMPLES:
    # Show status
    needle heartbeat status

    # JSON for monitoring
    needle heartbeat status --json
```

#### `needle config --help`

```
$ needle config --help

View or edit NEEDLE configuration

Display current configuration or open editor for modifications.

USAGE:
    needle config [COMMAND] [OPTIONS]

COMMANDS:
    show        Display current configuration (default)
    edit        Open configuration in editor
    validate    Validate configuration syntax
    path        Show configuration file paths

OPTIONS:
    --global            Target global config (~/.needle/config.yaml)
    --workspace         Target workspace config (.needle.yaml)

    -j, --json          Output as JSON (for 'show')
    -h, --help          Print help information

EXAMPLES:
    # Show current config
    needle config show

    # Edit global config
    needle config edit --global

    # Validate configuration
    needle config validate

    # Show config file paths
    needle config path
```

### Agent CLI Installation

Agent CLIs (claude, opencode, codex, aider) are **not auto-installed** because:
1. Each requires authentication/API keys
2. Users choose which agents they want
3. Installation methods vary (npm, pip, go, cargo)

NEEDLE prompts when a configured agent is missing:

```
$ needle run --agent=claude-anthropic-sonnet --workspace=/path
Error: Agent CLI 'claude' not found in PATH

Install with: npm install -g @anthropic-ai/claude-code

After installing, authenticate with: claude auth
```

---

## Process Management: Self-Invoking tmux

NEEDLE manages its own tmux sessions. When invoked, it detects whether it's already in tmux and acts accordingly:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         NEEDLE LAUNCH FLOW                                  │
│                                                                             │
│   $ needle run --workspace=/path --agent=claude-sonnet                      │
│                              │                                              │
│                              ▼                                              │
│                    ┌─────────────────────┐                                  │
│                    │  In tmux already?   │                                  │
│                    └──────────┬──────────┘                                  │
│                               │                                             │
│              ┌────────────────┴────────────────┐                            │
│              │ NO                              │ YES                        │
│              ▼                                 ▼                            │
│   ┌──────────────────────┐          ┌──────────────────────┐               │
│   │ Create tmux session  │          │ Run worker loop      │               │
│   │ needle-{name}        │          │ directly             │               │
│   │ Re-exec with --_tmux │          │                      │               │
│   └──────────────────────┘          └──────────────────────┘               │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Launch Commands

```bash
# Launch a single worker (auto-creates tmux session)
# Creates: needle-claude-anthropic-sonnet-alpha
needle run --workspace=/home/coder/project --agent=claude-anthropic-sonnet

# Launch multiple workers
# Creates: needle-claude-anthropic-sonnet-alpha, needle-claude-anthropic-sonnet-bravo, ...
needle run --workspace=/home/coder/project --agent=claude-anthropic-sonnet --count=3

# Launch with different agents
needle run --workspace=/home/coder/project --agent=opencode-alibaba-qwen
needle run --workspace=/home/coder/project --agent=codex-openai-gpt4
needle run --workspace=/home/coder/project --agent=aider-ollama-deepseek

# Launch with custom identifier (override NATO alphabet)
needle run --workspace=/home/coder/project --agent=claude-anthropic-opus --id=primary

# Run directly without tmux (for debugging)
needle run --workspace=/home/coder/project --agent=claude-anthropic-sonnet --no-tmux
```

### Session Naming Convention

Worker session names follow a configurable format using placeholders:

```yaml
# ~/.needle/config.yaml
naming:
  # Default pattern - captures full execution context
  pattern: "needle-{runner}-{provider}-{model}-{identifier}"

  # Alternative patterns:
  # pattern: "{runner}-{model}-{identifier}"      # Shorter
  # pattern: "w-{agent}-{identifier}"             # Compact
  # pattern: "{workspace_name}-{agent}-{n}"       # Workspace-prefixed
```

**Available Placeholders:**

| Placeholder | Description | Examples |
|-------------|-------------|----------|
| `{runner}` | Headless CLI type | `claude`, `opencode`, `codex`, `aider` |
| `{provider}` | Model provider | `anthropic`, `openai`, `alibaba`, `zai`, `ollama` |
| `{model}` | Specific model | `sonnet`, `opus`, `gpt4`, `qwen`, `deepseek` |
| `{agent}` | Full agent name | `claude-anthropic-sonnet`, `opencode-alibaba-qwen` |
| `{identifier}` | NATO alphabet | `alpha`, `bravo`, `charlie`, ... `zulu` |
| `{n}` | Numeric index | `1`, `2`, `3`, ... |
| `{workspace_name}` | Basename of workspace path | `project-a`, `my-app` |
| `{timestamp}` | Launch timestamp | `20260301-143022` |

**Default Pattern Examples:**
```
needle-claude-anthropic-sonnet-alpha
needle-claude-anthropic-opus-bravo
needle-opencode-alibaba-qwen-charlie
needle-codex-openai-gpt4-delta
needle-aider-ollama-deepseek-echo
needle-opencode-zai-glm5-foxtrot
```

**Alternative Pattern Examples:**

```yaml
# Compact pattern: "{runner}-{model}-{identifier}"
claude-sonnet-alpha
opencode-qwen-bravo

# Numeric pattern: "needle-{agent}-{n}"
needle-claude-anthropic-sonnet-1
needle-claude-anthropic-sonnet-2

# Workspace pattern: "{workspace_name}-{runner}-{identifier}"
my-project-claude-alpha
api-service-opencode-bravo
```

When `--count=N` is used, NEEDLE finds the next N available identifiers for the given pattern.

### Worker Management

```bash
# List all NEEDLE workers
needle list
# Output:
#   needle-claude-anthropic-sonnet-alpha    running   /home/coder/project   5 beads
#   needle-claude-anthropic-sonnet-bravo    running   /home/coder/project   3 beads
#   needle-opencode-alibaba-qwen-charlie    idle      /home/coder/other     0 beads

# List workers filtered by runner/provider/model
needle list --runner=claude
needle list --provider=anthropic
needle list --model=sonnet

# Attach to a worker (use full name or unique suffix)
needle attach claude-anthropic-sonnet-alpha
needle attach alpha    # works if unambiguous

# View worker output
needle logs claude-anthropic-sonnet-alpha
needle logs alpha --lines=200

# Stop a worker
needle stop claude-anthropic-sonnet-alpha
needle stop alpha

# Stop all workers of a specific type
needle stop --runner=claude
needle stop --provider=anthropic
needle stop --model=sonnet

# Stop all workers
needle stop --all
```

---

## Architecture

### Core Principle: Decentralized Parallel Workers

**Multiple NEEDLE workers run independently without external orchestration.** Each worker:
1. Selects a bead using the priority system
2. Claims it atomically (competing with other workers)
3. Pushes the prompt to a configurable headless agent
4. Waits for the agent to execute and exit
5. Processes the result and loops

```
┌───────────────────────────────────────────────────────────────────────────────────┐
│                            PARALLEL NEEDLE WORKERS                                │
│                         (No Central Orchestration)                                │
│                                                                                   │
│  ┌──────────────────────────────┐  ┌──────────────────────────────┐              │
│  │ needle-claude-anthropic-     │  │ needle-claude-anthropic-     │              │
│  │ sonnet-alpha                 │  │ sonnet-bravo                 │    ...       │
│  │ (tmux session)               │  │ (tmux session)               │              │
│  └──────────────┬───────────────┘  └──────────────┬───────────────┘              │
│                 │                                  │                              │
│  ┌──────────────────────────────┐  ┌──────────────────────────────┐              │
│  │ needle-opencode-alibaba-     │  │ needle-codex-openai-         │              │
│  │ qwen-charlie                 │  │ gpt4-delta                   │    ...       │
│  │ (tmux session)               │  │ (tmux session)               │              │
│  └──────────────┬───────────────┘  └──────────────┬───────────────┘              │
│                 │                                  │                              │
│                 └──────────────────┬───────────────┘                              │
│                                    │                                              │
│                                    ▼                                              │
│                    ┌───────────────────────────────┐                              │
│                    │   SHARED BEAD STATE (.beads)  │                              │
│                    │   - Atomic claims (br --claim)│                              │
│                    │   - Priority queue            │                              │
│                    │   - Status tracking           │                              │
│                    └───────────────────────────────┘                              │
└───────────────────────────────────────────────────────────────────────────────────┘
```

### Single Worker Loop

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         NEEDLE WORKER LOOP                                  │
│                                                                             │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │                       NEEDLE RUNNER                                 │   │
│   │                                                                     │   │
│   │   1. Priority Engine: Select next action (P1 → P2 → ... → P6)     │   │
│   │   2. Bead Manager: Claim bead atomically                           │   │
│   │   3. Prompt Builder: Construct prompt with bead context            │   │
│   │   4. Agent Dispatcher: Load agent config, render invoke template   │   │
│   └──────────────────────────────────────────────────────────────────┬──┘   │
│                                                                      │      │
│                                                                      ▼      │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │                    BASH EXECUTION                                   │   │
│   │                                                                     │   │
│   │   bash -c '<rendered invoke template with prompt injected>'         │   │
│   │                                                                     │   │
│   │   Example:                                                          │   │
│   │   bash -c 'cd /home/coder/project && claude --print <<NEEDLE_PROMPT │   │
│   │   Fix the bug in auth.py...                                         │   │
│   │   NEEDLE_PROMPT'                                                    │   │
│   │                                                                     │   │
│   │   → Headless CLI executes prompt                                    │   │
│   │   → Writes result to stdout                                         │   │
│   │   → EXITS immediately                                               │   │
│   └──────────────────────────────────────────────────────────────────┬──┘   │
│                                                                      │      │
│                                                                      ▼      │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │                    NEEDLE RUNNER (continued)                        │   │
│   │                                                                     │   │
│   │   5. Result Processor: Capture stdout, parse agent output          │   │
│   │   6. Bead Manager: Update status (completed/failed based on exit)  │   │
│   │   7. Effort Logger: Record tokens, time, cost                      │   │
│   │   8. LOOP → back to step 1                                         │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Strand System

NEEDLE implements a 7-strand system for finding work. The needle follows each strand in sequence until it finds a bead to work on. When no work is found on a strand, it falls through to the next.

| Strand | Name | Invokes Agent | Purpose |
|--------|------|---------------|---------|
| **Strand 1** | `pluck` | **Yes** | Pluck beads from configured workspaces |
| **Strand 2** | `explore` | No | Look for work in other workspaces |
| **Strand 3** | `mend` | No | Maintenance and cleanup |
| **Strand 4** | `weave` | **Yes** | Create beads from documentation gaps (opt-in) |
| **Strand 5** | `unravel` | **Yes** | Create alternatives for blocked HUMAN beads (opt-in) |
| **Strand 6** | `pulse` | **Yes** | Codebase health monitoring, auto-generate beads (opt-in) |
| **Strand 7** | `knot` | No | Alert human when stuck |

### Strand 1: Pluck (Primary Work)

**Invokes Agent:** Yes - executes the bead's task

**Purpose:** Pluck beads from configured workspaces

- Collect unassigned OPEN beads via `br ready --unassigned`
- Apply weighted selection: P0=10x, P1=5x, P2=2x, P3+=1x
- Claim bead atomically via `br update <id> --claim --actor <worker>`
- **Check for mitosis** - if bead is too complex, decompose before execution
- Build prompt with bead context
- Dispatch to agent
- Process result and update bead status

#### Bead Mitosis (Automatic Decomposition)

When a worker claims a bead, it first checks if the bead represents multiple tasks. If so, it **splits the bead into child beads** before execution. This enables parallelism and improves success rates (smaller beads are more likely to succeed).

**Mitosis Heuristics:**

| Heuristic | Threshold | Action |
|-----------|-----------|--------|
| **File count** | >5 files mentioned | Split by file or file group |
| **Unrelated concerns** | Multiple distinct tasks | Split by concern |
| **Explicit markers** | "and", "also", numbered items | Split at boundaries |
| **Size estimate** | >500 lines estimated change | Split into phases |

**Mitosis Flow:**

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        BEAD MITOSIS FLOW                                    │
│                                                                             │
│   Worker claims bead bd-001 "Implement auth system"                         │
│                              │                                              │
│                              ▼                                              │
│                    ┌─────────────────────┐                                  │
│                    │  Analyze complexity │                                  │
│                    │  (pre-execution)    │                                  │
│                    └──────────┬──────────┘                                  │
│                               │                                             │
│              ┌────────────────┴────────────────┐                            │
│              │ SIMPLE                          │ COMPLEX                    │
│              │ (single task)                   │ (multiple tasks)           │
│              ▼                                 ▼                            │
│   ┌──────────────────────┐          ┌──────────────────────┐               │
│   │ Execute normally     │          │ MITOSIS: Split bead  │               │
│   │                      │          │                      │               │
│   └──────────────────────┘          │ 1. Create child beads│               │
│                                     │ 2. Set dependencies  │               │
│                                     │ 3. Mark parent BLOCKED│              │
│                                     │ 4. Release claim     │               │
│                                     └──────────────────────┘               │
│                                                                             │
│   Result of mitosis:                                                        │
│                                                                             │
│   bd-001 "Implement auth system" [BLOCKED by bd-001a, bd-001b, bd-001c]    │
│     ├─ bd-001a "Add User model and migrations" [OPEN]                      │
│     ├─ bd-001b "Implement JWT token generation" [OPEN, blocked by bd-001a] │
│     └─ bd-001c "Add login/logout endpoints" [OPEN, blocked by bd-001b]     │
│                                                                             │
│   Child beads can now be worked in parallel (respecting dependencies)       │
│   Parent auto-completes when all children complete                          │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Mitosis Prompt Template:**

```
You are analyzing a task to determine if it should be broken down.

## Task
ID: {{BEAD_ID}}
Title: {{BEAD_TITLE}}
Description: {{BEAD_DESCRIPTION}}

## Workspace Context
Files that may be affected: {{RELEVANT_FILES}}
Existing related code: {{CODE_CONTEXT}}

## Instructions

1. Analyze whether this task represents a SINGLE atomic task or MULTIPLE distinct tasks
2. A task should be split if ANY of these apply:
   - It mentions multiple unrelated concerns (e.g., "Add auth AND fix logging")
   - It would require changes to more than 5 unrelated files
   - It contains numbered steps or "and/also" that indicate separate work items
   - It would take more than ~500 lines of changes

3. If SINGLE task (no split needed):
   {"mitosis": false, "reason": "Brief explanation why this is atomic"}

4. If MULTIPLE tasks (split needed):
   {
     "mitosis": true,
     "children": [
       {
         "title": "Child task title",
         "description": "What this child task accomplishes",
         "files": ["list", "of", "files"],
         "blocked_by": []  // IDs of other children this depends on
       }
     ],
     "parent_title": "Updated parent title (becomes umbrella)",
     "reasoning": "Why this split makes sense"
   }

5. Guidelines for splitting:
   - Each child should be completable in a single agent session
   - Children should have clear boundaries (ideally by file or module)
   - Dependencies should form a DAG (no cycles)
   - Prefer fewer children (2-4) over many tiny tasks
```

**Mitosis Configuration:**

```yaml
# ~/.needle/config.yaml
mitosis:
  enabled: true          # Enable automatic decomposition
  max_children: 5        # Maximum children per split
  min_complexity: 3      # Minimum files/concerns before considering split

  # Disable for certain bead types
  skip_types:
    - bug              # Bugs should stay atomic
    - hotfix           # Urgent fixes stay atomic

  # Skip for certain labels
  skip_labels:
    - no-mitosis
    - atomic
```

**Telemetry Events:**

```jsonl
{"ts":"...","event":"bead.mitosis.check","session":"...","worker":{...},"data":{"bead_id":"bd-001","title":"Implement auth"}}
{"ts":"...","event":"bead.mitosis.triggered","session":"...","worker":{...},"data":{"bead_id":"bd-001","children_count":3,"reason":"multiple_concerns"}}
{"ts":"...","event":"bead.mitosis.child_created","session":"...","worker":{...},"data":{"parent_id":"bd-001","child_id":"bd-001a","title":"Add User model"}}
{"ts":"...","event":"bead.mitosis.complete","session":"...","worker":{...},"data":{"parent_id":"bd-001","children":["bd-001a","bd-001b","bd-001c"]}}
```

### Strand 2: Explore (Discover Work)

**Invokes Agent:** No - only spawns new NEEDLE workers

**Purpose:** Look for work in other workspaces

- When local workspaces exhausted (consecutive empty iterations)
- Traverse to parent directories to discover new beads workspaces
- Spawn additional NEEDLE workers if backlog exceeds threshold
- Each spawned worker is independent (same parallel model)

```bash
# Auto-scaling spawns new workers via:
needle run --workspace=$DISCOVERED_WS --agent=$AGENT --name=$NEXT_NAME
```

### Strand 3: Mend (Maintenance)

**Invokes Agent:** No - housekeeping only

**Purpose:** Keep the system healthy

- Clean orphaned claims (stale > 30min)
- Detect hung agents (execution time > threshold)
- Update worker health status file
- Report metrics

### Strand 4: Weave (Gap Analysis, Opt-in)

**Invokes Agent:** Yes - analyzes documentation to find gaps

**Purpose:** Create work from documentation

- When idle, scan documentation (ADRs, TODOs, ROADMAPs)
- Use agent to identify gaps between docs and implementation
- Create beads for identified gaps
- Requires explicit opt-in via config

**Weave Prompt Template:**

```
You are analyzing a codebase for gaps between documentation and implementation.

## Documentation Files
{{DOC_FILES}}

## Current Open Beads
{{OPEN_BEADS}}

## Instructions

1. Read the documentation files above (ADRs, TODOs, ROADMAPs, README)
2. Identify features, tasks, or fixes mentioned in docs that:
   - Are NOT already tracked as open beads
   - Are NOT already implemented in the codebase
   - Are actionable and well-defined enough to work on

3. For each gap found, output a JSON object:
{
  "gaps": [
    {
      "title": "Brief title for the bead",
      "description": "Detailed description of what needs to be done",
      "source_file": "path/to/doc/that/mentions/this",
      "source_line": "relevant quote from documentation",
      "priority": 2,  // 0=critical, 1=high, 2=medium, 3=low
      "type": "task|bug|feature",
      "estimated_effort": "small|medium|large"
    }
  ]
}

4. Only output gaps that are:
   - Clearly defined in documentation
   - Not duplicates of existing beads
   - Actually missing from implementation

5. If no gaps found, output: {"gaps": []}
```

### Strand 5: Unravel (HUMAN Alternatives, Opt-in)

**Invokes Agent:** Yes - creates alternative approaches for blocked beads

**Purpose:** Unblock parallelism when humans are slow

- Find HUMAN-type beads awaiting human input
- Use agent to create alternative solution beads
- Workers can implement alternatives while waiting for human
- Requires explicit opt-in via config

**Unravel Prompt Template:**

```
You are helping unblock a task that is waiting for human input.

## Blocked Bead
ID: {{BEAD_ID}}
Title: {{BEAD_TITLE}}
Description: {{BEAD_DESCRIPTION}}
Blocked Reason: {{BLOCKED_REASON}}
Waiting Since: {{WAITING_SINCE}}

## Context
{{RELEVANT_CODE_CONTEXT}}

## Instructions

The above bead is blocked waiting for human input. Your job is to propose
alternative approaches that could unblock progress while waiting.

1. Analyze why this bead is blocked
2. Propose 1-3 alternative approaches that:
   - Work around the blocker without the human decision
   - Are reversible or can be easily changed later
   - Make progress on the underlying goal
   - Are clearly labeled as "alternative pending human decision"

3. For each alternative, output:
{
  "alternatives": [
    {
      "title": "[ALTERNATIVE] Brief title",
      "description": "What this alternative does and why it's reasonable",
      "approach": "Detailed implementation approach",
      "reversibility": "How easily this can be changed when human decides",
      "tradeoffs": "What we gain vs what we risk",
      "priority": 2,
      "parent_bead": "{{BEAD_ID}}",
      "labels": ["alternative", "pending-human-review"]
    }
  ]
}

4. Do NOT propose alternatives that:
   - Make irreversible decisions the human should make
   - Contradict explicit requirements
   - Are just "wait longer"

5. If no reasonable alternatives exist, output: {"alternatives": [], "reason": "explanation"}
```

### Strand 6: Pulse (Codebase Health)

**Invokes Agent:** Yes - analyzes codebase for issues

**Purpose:** Proactive codebase monitoring and automatic bead generation

- Runs periodically when no other work is available
- Scans codebase for issues that should become beads
- Auto-generates beads for discovered problems
- Requires explicit opt-in via config

**What Pulse detects:**
- Security vulnerabilities (CVEs in dependencies)
- Stale/outdated dependencies beyond threshold
- Documentation drift (code changed, docs didn't)
- Test coverage gaps (new code without tests)
- TODO/FIXME/HACK comments older than configured age
- Dead code or unused exports
- Code smells flagged by configured linters

**Pulse Configuration:**

```yaml
strands:
  pulse:
    enabled: false  # Opt-in (default: off)
    frequency: 24h  # Minimum time between pulse scans
    max_beads_per_run: 5  # Cap beads created per scan

    detectors:
      security_scan: true
      dependency_freshness:
        enabled: true
        max_age_days: 90
      doc_drift: true
      coverage_gaps:
        enabled: true
        threshold: 60  # Warn if file coverage below 60%
      stale_todos:
        enabled: true
        max_age_days: 30
      dead_code: false  # Requires additional tooling
      linter_issues:
        enabled: true
        severity: error  # Only create beads for errors, not warnings
```

**Pulse Prompt Template:**

```
You are analyzing a codebase for issues that should be tracked and fixed.

## Workspace
Path: {{WORKSPACE_PATH}}
Last pulse scan: {{LAST_SCAN_TIME}}

## Detector Results

### Security Scan
{{SECURITY_SCAN_OUTPUT}}

### Dependency Freshness
{{DEPENDENCY_CHECK_OUTPUT}}

### Documentation Drift
{{DOC_DRIFT_OUTPUT}}

### Test Coverage
{{COVERAGE_OUTPUT}}

### Stale TODOs
{{TODO_SCAN_OUTPUT}}

### Linter Issues
{{LINTER_OUTPUT}}

## Current Open Beads
{{OPEN_BEADS}}

## Instructions

1. Review the detector outputs above
2. Identify issues that:
   - Are NOT already tracked as open beads
   - Are actionable and specific enough to fix
   - Have clear acceptance criteria
3. Prioritize by severity (security > bugs > maintenance)
4. Do NOT create duplicate beads for existing issues

For each issue, output:
{
  "issues": [
    {
      "title": "Brief, actionable title",
      "description": "What the issue is and how to fix it",
      "detector": "which detector found this",
      "severity": "critical|high|medium|low",
      "file_path": "path/to/affected/file",
      "priority": 1,
      "type": "security|bug|maintenance|docs",
      "labels": ["pulse-generated", "detector-name"]
    }
  ],
  "skipped": [
    {"reason": "why this issue was not converted to a bead"}
  ]
}
```

**Pulse State Tracking:**

```
~/.needle/state/pulse/
├── last_scan.json       # Timestamp and results of last scan
├── seen_issues.json     # Issues already converted to beads (dedup)
└── detector_cache/      # Cached detector outputs
    ├── security.json
    ├── dependencies.json
    └── coverage.json
```

### Strand 7: Knot (Alert Human)

**Invokes Agent:** No - only creates alert bead

**Purpose:** Error state escalation

- When truly stuck (no work for extended period across all strands)
- Create high-priority alert bead
- Notify via configured channels (if any)

---

## Component Design

### NEEDLE CLI (`needle`)

The main entry point is a single bash script with subcommands:

```bash
# Bootstrap & Setup
needle setup         # Check and install dependencies (auto-runs on first use)
needle setup --check # Check dependencies without installing
needle version       # Show needle and dependency versions
needle upgrade       # Download and install latest version
needle rollback      # Rollback to previous version

# Worker Management
needle run         # Start a worker (self-invokes into tmux)
needle list        # List running workers
needle attach      # Attach to a worker's tmux session
needle logs        # View worker logs
needle stop        # Stop worker(s)
needle status      # Show worker health and stats

# Agent Testing
needle test-agent  # Test an agent adapter
needle agents      # List available agent configurations
```

### Configuration Hierarchy

NEEDLE uses a layered configuration system:

1. **Global config** (`~/.needle/config.yaml`) - System-wide defaults and limits
2. **Workspace config** (`.needle.yaml` in workspace root) - Per-project overrides
3. **CLI flags** - Per-invocation overrides

### Global Configuration

```yaml
# ~/.needle/config.yaml - Global settings and limits

# Concurrency limits per provider-model combination
# Prevents overwhelming API rate limits or local resources
limits:
  # Provider-level limits (applies to all models from provider)
  providers:
    anthropic:
      max_concurrent: 5         # Max concurrent workers using any Anthropic model
      requests_per_minute: 60   # Rate limit (0 = unlimited)
    openai:
      max_concurrent: 3
      requests_per_minute: 60
    alibaba:
      max_concurrent: 10
    ollama:
      max_concurrent: 2         # Local inference - limited by hardware

  # Model-level limits (overrides provider limits)
  models:
    claude-anthropic-opus:
      max_concurrent: 2         # Opus is expensive, limit concurrent use
      requests_per_minute: 20
    claude-anthropic-sonnet:
      max_concurrent: 5
    opencode-ollama-deepseek:
      max_concurrent: 1         # Single local GPU

  # Global maximum across all workers
  global_max_concurrent: 20

# Default runner settings (can be overridden per-workspace)
runner:
  polling_interval: 2s
  idle_timeout: 300s
  max_consecutive_empty: 5

# Session naming pattern (see Session Naming Convention for placeholders)
naming:
  pattern: "needle-{runner}-{provider}-{model}-{identifier}"

# Strand settings (which strands the needle follows)
strands:
  pluck: true           # Strand 1: Pluck beads from workspaces
  explore: true         # Strand 2: Look for work in other workspaces
  mend: true            # Strand 3: Maintenance and cleanup
  weave: false          # Strand 4: Create beads from doc gaps (opt-in)
  unravel: false        # Strand 5: Create HUMAN alternatives (opt-in)
  pulse: false          # Strand 6: Codebase health monitoring (opt-in)
  knot: true            # Strand 7: Alert human when stuck

# Scaling behavior
scaling:
  spawn_threshold: 3            # Spawn new worker if backlog > N
  max_workers_per_agent: 10     # Max workers per agent type
  cooldown_seconds: 30          # Wait between spawn attempts

# Effort tracking and budgets
effort:
  log_dir: ~/.needle/logs
  budget:
    daily_limit_usd: 50.0       # Stop spawning when daily spend exceeds
    warning_threshold: 0.8      # Warn at 80% of limit
    per_bead_limit_usd: 5.0     # Max spend per single bead execution

# Billing model affects priority behavior
# "pay_per_token" - Conservative: minimize spend, use cheaper models when possible
# "use_or_lose"   - Aggressive: maximize utilization of subscription quota
# "unlimited"     - Local inference (ollama) - no cost constraints
billing:
  default_model: pay_per_token

  # Per-provider billing overrides
  providers:
    anthropic: pay_per_token    # API billing
    openai: pay_per_token       # API billing
    alibaba: use_or_lose        # Monthly subscription with quota
    zai: use_or_lose            # Monthly subscription with quota
    ollama: unlimited           # Local inference
```

### Billing Model Behavior

The billing model affects how NEEDLE prioritizes work and spawns workers:

| Model | Priority Behavior | Scaling Behavior |
|-------|-------------------|------------------|
| `pay_per_token` | Conservative - complete current beads before spawning more workers. Prefer cheaper models. | Scale slowly, respect budgets strictly |
| `use_or_lose` | Aggressive - maximize throughput to use monthly quota. Spawn workers eagerly. | Scale up quickly, fill quota before reset |
| `unlimited` | Balanced - optimize for task completion, no cost concerns | Scale based on hardware capacity |

**Strand adjustments by billing model:**

```yaml
# pay_per_token: Minimize spend
strands:
  explore: false    # Don't discover more work (saves tokens)
  weave: false      # Don't create speculative work
  unravel: false    # Don't do redundant work

# use_or_lose: Maximize utilization
strands:
  explore: true     # Find more work to consume quota
  weave: true       # Create work to fill idle time
  unravel: true     # Do alternative work while waiting

# unlimited: Balanced
strands:
  explore: true
  weave: false      # Still opt-in (speculative)
  unravel: true
```

### Workspace Configuration

```yaml
# .needle.yaml - Per-workspace settings (in workspace root)

# Override default workspaces for this project
workspaces:
  - .                           # Current workspace
  - ../shared-lib               # Related workspace

# Override runner settings for this workspace
runner:
  polling_interval: 5s          # Slower polling for this project

# Workspace-specific agent preferences
preferred_agents:
  - claude-anthropic-sonnet     # Try sonnet first
  - opencode-alibaba-qwen       # Fallback to qwen

# Workspace-specific budget (lower than global)
effort:
  budget:
    daily_limit_usd: 10.0
```

### Concurrency Enforcement

When launching workers, NEEDLE checks global limits:

```bash
$ needle run --agent=claude-anthropic-opus --workspace=/path
Error: Concurrency limit reached for claude-anthropic-opus (2/2 workers running)

Currently running opus workers:
  needle-claude-anthropic-opus-alpha  /home/coder/project-a  bd-xyz (15m)
  needle-claude-anthropic-opus-bravo  /home/coder/project-b  bd-abc (3m)

Options:
  --wait     Wait for a slot to become available
  --force    Override limit (use with caution)
```

Workers track their state in `~/.needle/state/`:

```
~/.needle/state/
├── workers.json              # Active worker registry
└── rate_limits/
    ├── anthropic.json        # Request timestamps for rate limiting
    └── openai.json
```

---

## Hook System

NEEDLE provides user-definable bash scripts that execute at lifecycle events. This enables infinite customization without modifying core code.

### Hook Points

| Hook | Trigger | Use Cases |
|------|---------|-----------|
| `pre_claim` | Before attempting to claim a bead | Custom filtering, workspace preparation |
| `post_claim` | After successful claim | Notifications, logging, setup |
| `pre_execute` | Before agent invocation | Environment setup, pre-checks |
| `post_execute` | After agent exits (success or failure) | Cleanup, notifications, custom validation |
| `pre_complete` | Before marking bead complete | Quality gates, additional validation |
| `post_complete` | After bead marked complete | Notifications, cleanup, metrics |
| `on_failure` | When bead execution fails | Alerting, debugging, custom retry logic |
| `on_quarantine` | When bead gets quarantined | Alerting, escalation |

### Hook Configuration

```yaml
# ~/.needle/config.yaml
hooks:
  # Global hooks (apply to all workers)
  pre_claim: ~/.needle/hooks/pre-claim.sh
  post_claim: ~/.needle/hooks/post-claim.sh
  pre_execute: ~/.needle/hooks/pre-execute.sh
  post_execute: ~/.needle/hooks/post-execute.sh
  pre_complete: ~/.needle/hooks/pre-complete.sh
  post_complete: ~/.needle/hooks/post-complete.sh
  on_failure: ~/.needle/hooks/on-failure.sh
  on_quarantine: ~/.needle/hooks/on-quarantine.sh

  # Hook execution settings
  timeout: 30s          # Max time for hook execution
  fail_action: warn     # warn | abort | ignore
```

```yaml
# .needle.yaml (workspace-level hooks override global)
hooks:
  pre_execute: .needle/hooks/setup-env.sh
  post_complete: .needle/hooks/deploy-preview.sh
```

### Hook Environment Variables

All hooks receive context via environment variables:

```bash
#!/bin/bash
# Example hook: ~/.needle/hooks/post-execute.sh

# Worker context
echo "Worker: $NEEDLE_WORKER"           # needle-claude-anthropic-sonnet-alpha
echo "Session: $NEEDLE_SESSION"         # Full session name
echo "PID: $NEEDLE_PID"                 # Worker process ID

# Bead context
echo "Bead ID: $NEEDLE_BEAD_ID"         # bd-123
echo "Bead Title: $NEEDLE_BEAD_TITLE"   # Fix authentication bug
echo "Bead Priority: $NEEDLE_BEAD_PRIORITY"  # 1
echo "Bead Type: $NEEDLE_BEAD_TYPE"     # bug

# Execution context
echo "Workspace: $NEEDLE_WORKSPACE"     # /home/coder/project
echo "Agent: $NEEDLE_AGENT"             # claude-anthropic-sonnet
echo "Exit Code: $NEEDLE_EXIT_CODE"     # 0
echo "Duration: $NEEDLE_DURATION_MS"    # 45000

# Strand context
echo "Strand: $NEEDLE_STRAND"           # 1
echo "Strand Name: $NEEDLE_STRAND_NAME" # pluck

# File changes (if available)
echo "Files Changed: $NEEDLE_FILES_CHANGED"  # 3
echo "Lines Added: $NEEDLE_LINES_ADDED"      # 45
echo "Lines Removed: $NEEDLE_LINES_REMOVED"  # 12

# Agent output (path to temp file with full output)
echo "Output File: $NEEDLE_OUTPUT_FILE" # /tmp/needle-output-xxxxx
```

### Hook Exit Codes

Hooks can influence NEEDLE behavior via exit codes:

| Exit Code | Meaning | Behavior |
|-----------|---------|----------|
| `0` | Success | Continue normally |
| `1` | Warning | Log warning, continue |
| `2` | Abort | Abort current operation (release bead if claimed) |
| `3` | Skip | Skip to next strand (don't claim this bead) |

### Example Hooks

**Slack notification on completion:**
```bash
#!/bin/bash
# ~/.needle/hooks/post-complete.sh

if [[ $NEEDLE_EXIT_CODE -eq 0 ]]; then
  curl -s -X POST "$SLACK_WEBHOOK" \
    -H "Content-Type: application/json" \
    -d "{
      \"text\": \"✅ *$NEEDLE_BEAD_TITLE* completed\",
      \"blocks\": [{
        \"type\": \"section\",
        \"text\": {
          \"type\": \"mrkdwn\",
          \"text\": \"*Bead:* $NEEDLE_BEAD_ID\\n*Worker:* $NEEDLE_WORKER\\n*Duration:* ${NEEDLE_DURATION_MS}ms\"
        }
      }]
    }"
fi
```

**Quality gate before completion:**
```bash
#!/bin/bash
# ~/.needle/hooks/pre-complete.sh

cd "$NEEDLE_WORKSPACE" || exit 2

# Run tests
if ! npm test &>/dev/null; then
  echo "ERROR: Tests failed, blocking completion"
  exit 2  # Abort - don't mark complete
fi

# Run linting
if ! npm run lint &>/dev/null; then
  echo "WARNING: Lint issues found"
  exit 1  # Warning - continue but log
fi

exit 0
```

**Custom filtering in pre-claim:**
```bash
#!/bin/bash
# ~/.needle/hooks/pre-claim.sh

# Skip beads with certain labels
if echo "$NEEDLE_BEAD_LABELS" | grep -q "needs-human"; then
  echo "Skipping bead with needs-human label"
  exit 3  # Skip to next bead
fi

exit 0
```

### Hook Telemetry

```jsonl
{"ts":"...","event":"hook.started","session":"...","data":{"hook":"post_execute","bead_id":"bd-123"}}
{"ts":"...","event":"hook.completed","session":"...","data":{"hook":"post_execute","bead_id":"bd-123","exit_code":0,"duration_ms":1200}}
{"ts":"...","event":"hook.failed","session":"...","data":{"hook":"pre_complete","bead_id":"bd-123","exit_code":2,"action":"abort"}}
```

---

## File Collision Management

When multiple workers operate on the same codebase, file write conflicts can occur. NEEDLE provides a checkout system using `/dev/shm` for fast, volatile file locks that automatically convert conflicts into bead dependencies.

### Design Principles

1. **No blocking** - Workers never wait for file locks; conflicts become dependencies
2. **Self-healing** - Closing a bead releases all its file claims automatically
3. **Cross-workspace** - All NEEDLE workers share the same lock namespace
4. **Volatile** - Locks live in `/dev/shm` (RAM), no stale locks after reboot

### Lock Structure

```
/dev/shm/needle-locks/
├── {md5(filepath)}.lock/       # Directory (mkdir for atomicity)
│   └── info                    # JSON metadata
```

**Lock info format:**
```json
{
  "bead": "nd-2ov",
  "worker": "claude-code-glm-5-alpha",
  "path": "/home/coder/NEEDLE/src/cli/run.sh",
  "ts": 1709337600,
  "workspace": "/home/coder/NEEDLE"
}
```

### Checkout Workflow

```
Worker attempts file write (Edit tool)
    ↓
checkout_file() checks /dev/shm/needle-locks/{hash}.lock
    ↓
┌─ Lock free ──────────────────────┐    ┌─ Lock held by bead X ────────────┐
│ 1. mkdir lock dir (atomic)       │    │ 1. Read blocking bead ID         │
│ 2. Write info JSON               │    │ 2. br dep add current-bead X     │
│ 3. Proceed with file write       │    │ 3. br update current --status open│
│ 4. (on bead close) release lock  │    │ 4. Worker abandons, finds other  │
└──────────────────────────────────┘    │    work from ready queue         │
                                        └───────────────────────────────────┘
```

### API (src/lock/checkout.sh)

```bash
# Attempt to checkout a file for writing
# Returns: 0 = acquired, 1 = blocked (prints blocking bead)
checkout_file "$filepath" "$bead_id" "$worker_id"

# Release a specific file lock
release_file "$filepath"

# Release ALL locks held by a bead (called on bead close)
release_bead_locks "$bead_id"

# Check if file is locked (read-only query)
# Returns: 0 = locked (prints info), 1 = free
check_file "$filepath"

# List all current locks (debugging)
list_locks
```

### Integration with Agent Tools

NEEDLE intercepts file write operations via Claude Code hooks:

```json
// Agent settings.json (injected by NEEDLE)
{
  "hooks": {
    "preToolUse": [
      {
        "matcher": "Edit|Write",
        "hook": "~/.needle/hooks/file-checkout.sh"
      }
    ]
  }
}
```

**Hook script (file-checkout.sh):**
```bash
#!/bin/bash
# Called before Edit/Write tool execution
# Receives: $TOOL_NAME, $TOOL_INPUT (JSON)

filepath=$(echo "$TOOL_INPUT" | jq -r '.file_path // .path')
bead_id="$NEEDLE_BEAD_ID"
worker_id="$NEEDLE_WORKER"

if ! checkout_file "$filepath" "$bead_id" "$worker_id"; then
    blocking_bead=$(check_file "$filepath" | jq -r '.bead')
    echo "FILE_CONFLICT: $filepath locked by $blocking_bead"

    # Add dependency and abort this bead's execution
    br dep add "$bead_id" "$blocking_bead"
    exit 1  # Signals NEEDLE to re-queue this bead
fi
```

### Bead Completion Hook

When a bead is closed (success or failure), all its file claims are released:

```bash
# ~/.needle/hooks/post-complete.sh (or on_failure.sh)
release_bead_locks "$NEEDLE_BEAD_ID"
```

This ensures:
- Completed work releases locks immediately
- Failed beads don't hold locks forever
- Quarantined beads release locks for other workers

### Stale Lock Detection

Locks older than the configured timeout (default: 30 minutes) can be forcibly released:

```yaml
# ~/.needle/config.yaml
file_locks:
  timeout: 30m        # Max lock age before considered stale
  stale_action: warn  # warn | release | ignore
```

**Stale lock handling:**
```bash
check_stale_locks() {
    local now=$(date +%s)
    local timeout=1800  # 30 minutes

    for lock in /dev/shm/needle-locks/*.lock; do
        local ts=$(jq -r '.ts' "$lock/info")
        if (( now - ts > timeout )); then
            local bead=$(jq -r '.bead' "$lock/info")
            log_warn "Stale lock: $lock (held by $bead for $((now - ts))s)"
            # Optionally: rm -rf "$lock"
        fi
    done
}
```

### Cross-Workspace Coordination

Since `/dev/shm` is shared across all processes, workers from different workspaces (NEEDLE, FABRIC, etc.) see the same locks:

```
NEEDLE worker (nd-2ov) checks out:  /home/coder/shared-lib/utils.ts
FABRIC worker (bd-muv) attempts:    /home/coder/shared-lib/utils.ts
    ↓
FABRIC sees lock, adds dependency: br dep add bd-muv nd-2ov
    ↓
FABRIC worker moves to other work
    ↓
NEEDLE completes nd-2ov, releases lock
    ↓
bd-muv becomes unblocked, FABRIC worker picks it up
```

### Telemetry

```jsonl
{"ts":"...","event":"file.checkout","session":"...","data":{"bead":"nd-2ov","path":"/src/cli/run.sh","status":"acquired"}}
{"ts":"...","event":"file.conflict","session":"...","data":{"bead":"bd-muv","path":"/src/cli/run.sh","blocked_by":"nd-2ov"}}
{"ts":"...","event":"file.release","session":"...","data":{"bead":"nd-2ov","path":"/src/cli/run.sh","held_for_ms":45000}}
{"ts":"...","event":"file.stale","session":"...","data":{"bead":"nd-xxx","path":"/src/old.sh","age_s":3600,"action":"released"}}
```

### Multi-Agent Interception Strategies

Claude Code supports hooks natively, but other agents (OpenCode, Codex, Aider) do not. NEEDLE provides multiple interception strategies with increasing enforcement levels:

#### Strategy 1: Prompt Injection (Soft Enforcement)

Inject checkout instructions into the agent prompt:

```bash
# Prepended to every bead prompt sent to agent
NEEDLE_CHECKOUT_INSTRUCTIONS="
IMPORTANT: Before editing ANY file, you MUST run:
    needle checkout <filepath>

If checkout fails, DO NOT edit that file. Instead run:
    needle status <filepath>
to see which bead has it checked out, then move to other work.

After completing edits, run:
    needle release <filepath>
"
```

**Pros:** Works with any LLM-based agent, zero setup
**Cons:** Relies on instruction-following (~90% compliance)

#### Strategy 2: Post-Execution Reconciliation (Reactive)

Detect conflicts after agent execution and rollback:

```bash
# ~/.needle/hooks/post-execute.sh
detect_file_conflicts() {
    local changed_files=$(git diff --name-only HEAD)
    local conflicts=0

    for file in $changed_files; do
        lock_info=$(check_file "$file")
        if [ -n "$lock_info" ]; then
            blocking_bead=$(echo "$lock_info" | jq -r '.bead')

            if [ "$blocking_bead" != "$NEEDLE_BEAD_ID" ]; then
                log_warn "CONFLICT: $file was edited but locked by $blocking_bead"

                # Rollback this file
                git checkout HEAD -- "$file"

                # Add dependency
                br dep add "$NEEDLE_BEAD_ID" "$blocking_bead"

                # Emit conflict metric
                emit_event "file.conflict.missed" "{
                    \"bead\": \"$NEEDLE_BEAD_ID\",
                    \"blocking_bead\": \"$blocking_bead\",
                    \"path\": \"$file\",
                    \"strategy\": \"post_exec_rollback\"
                }"

                ((conflicts++))
            fi
        fi
    done

    if [ $conflicts -gt 0 ]; then
        log_error "$conflicts file conflicts detected and rolled back"
        br update "$NEEDLE_BEAD_ID" --status open  # Re-queue
        return 1
    fi
}
```

**Pros:** Catches everything, guaranteed consistency
**Cons:** Wasted agent execution time on conflicts

#### Strategy 3: LD_PRELOAD Shim (Hard Enforcement)

Intercept file system calls at the libc level:

```c
// src/lock/libcheckout.c
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>

static int (*real_open)(const char *, int, ...) = NULL;
static int (*real_openat)(int, const char *, int, ...) = NULL;

static int check_needle_lock(const char *path) {
    // Shell out to checkout check (or use shared memory directly)
    char cmd[4096];
    snprintf(cmd, sizeof(cmd), "needle check-lock '%s' 2>/dev/null", path);
    return system(cmd) == 0;  // 0 = locked, non-zero = free
}

int open(const char *path, int flags, ...) {
    if (!real_open) real_open = dlsym(RTLD_NEXT, "open");

    // Check write operations against lock
    if (flags & (O_WRONLY | O_RDWR | O_CREAT | O_TRUNC)) {
        if (check_needle_lock(path)) {
            errno = EACCES;
            return -1;  // Block the write
        }
    }

    // Pass through to real open
    va_list args;
    va_start(args, flags);
    mode_t mode = va_arg(args, mode_t);
    va_end(args);
    return real_open(path, flags, mode);
}
```

**Build and use:**
```bash
gcc -shared -fPIC -o ~/.needle/lib/libcheckout.so src/lock/libcheckout.c -ldl

# Launch agent with interception
LD_PRELOAD=~/.needle/lib/libcheckout.so opencode --prompt "$prompt"
```

**Pros:** Guaranteed enforcement, works with any agent/tool
**Cons:** Requires compilation, may interfere with some tools

#### Strategy 4: fanotify (Kernel-Level)

Use Linux fanotify for permission-based file access control:

```bash
# Requires CAP_SYS_ADMIN or root
needle-fanotify-daemon --watch="$NEEDLE_WORKSPACE" --lock-dir=/dev/shm/needle-locks
```

**Pros:** Kernel-enforced, zero overhead for allowed operations
**Cons:** Requires elevated privileges, Linux-only

#### Recommended Configuration

```yaml
# ~/.needle/config.yaml
file_locks:
  # Enforcement strategy (layered)
  strategies:
    - prompt_injection    # Always on (soft)
    - post_exec_rollback  # Always on (safety net)
    - ld_preload: false   # Opt-in (hard enforcement)
    - fanotify: false     # Opt-in (requires privileges)

  # Strategy-specific settings
  prompt_injection:
    include_in_prompt: true
    cli_tool: "needle checkout"

  post_exec_rollback:
    enabled: true
    auto_dependency: true
    requeue_on_conflict: true

  ld_preload:
    library: ~/.needle/lib/libcheckout.so
    agents: [opencode, aider]  # Only for these agents
```

### Collision Metrics & Effectiveness Measurement

To evaluate strategy effectiveness, NEEDLE tracks detailed collision metrics:

#### Metric Events

```jsonl
{"ts":"...","event":"file.checkout.attempt","data":{"bead":"nd-2ov","path":"/src/run.sh","strategy":"prompt"}}
{"ts":"...","event":"file.checkout.acquired","data":{"bead":"nd-2ov","path":"/src/run.sh"}}
{"ts":"...","event":"file.checkout.blocked","data":{"bead":"bd-muv","path":"/src/run.sh","blocked_by":"nd-2ov","strategy":"prompt"}}
{"ts":"...","event":"file.conflict.missed","data":{"bead":"bd-xyz","path":"/src/run.sh","blocked_by":"nd-2ov","strategy":"post_exec_rollback"}}
{"ts":"...","event":"file.conflict.prevented","data":{"bead":"bd-xyz","path":"/src/run.sh","blocked_by":"nd-2ov","strategy":"ld_preload"}}
```

#### Metrics Aggregation

```bash
# ~/.needle/state/metrics/file_collisions.json
{
  "period": "2026-03-07T00:00:00Z/2026-03-08T00:00:00Z",
  "totals": {
    "checkout_attempts": 1247,
    "checkouts_acquired": 1180,
    "checkouts_blocked": 67,
    "conflicts_missed": 3,        # Got through, caught by post_exec
    "conflicts_prevented": 64     # Blocked by enforcement
  },
  "by_strategy": {
    "prompt_injection": {
      "attempts": 800,
      "blocked": 45,
      "missed": 3,
      "effectiveness": 0.937      # 45/(45+3) = 93.7%
    },
    "post_exec_rollback": {
      "caught": 3,
      "rollbacks": 3
    },
    "ld_preload": {
      "attempts": 447,
      "blocked": 22,
      "missed": 0,
      "effectiveness": 1.0        # 100%
    }
  },
  "hot_files": [
    {"path": "/src/cli/run.sh", "conflicts": 12},
    {"path": "/src/agent/dispatch.sh", "conflicts": 8},
    {"path": "/README.md", "conflicts": 5}
  ],
  "conflict_pairs": [
    {"bead_a": "nd-2ov", "bead_b": "bd-muv", "file": "/src/cli/run.sh", "count": 3}
  ]
}
```

#### Dashboard Metrics (FABRIC integration)

FABRIC can display live collision metrics:

```
┌─ File Lock Status ────────────────────────────────────────────┐
│ Active Locks: 4        Conflicts Today: 12      Missed: 1    │
├───────────────────────────────────────────────────────────────┤
│ Strategy Effectiveness (24h):                                 │
│   prompt_injection:  ████████████████████░░░░  89.2%         │
│   ld_preload:        █████████████████████████ 100%          │
│   post_exec_rollback: caught 2 missed conflicts              │
├───────────────────────────────────────────────────────────────┤
│ Hot Files:                                                    │
│   /src/cli/run.sh         ████████████  12 conflicts         │
│   /src/agent/dispatch.sh  ████████      8 conflicts          │
│   /README.md              █████         5 conflicts          │
└───────────────────────────────────────────────────────────────┘
```

#### Effectiveness Analysis Commands

```bash
# View collision summary
needle metrics collisions --period=24h

# Identify hot files (frequently contested)
needle metrics hot-files --top=10

# Compare strategy effectiveness
needle metrics strategies --compare

# Export for analysis
needle metrics export --format=csv --output=collisions.csv
```

#### Automated Tuning

Based on metrics, NEEDLE can recommend configuration changes:

```bash
$ needle metrics recommend

📊 Collision Analysis (last 7 days):

⚠️  prompt_injection effectiveness: 89.2% (below 95% threshold)
   Recommendation: Enable ld_preload for agents: opencode, aider

⚠️  Hot file detected: /src/cli/run.sh (47 conflicts)
   Recommendation: Consider splitting into smaller modules

✓  post_exec_rollback caught 8 missed conflicts
   These would have caused merge conflicts without this safety net

Suggested config changes:
  file_locks.ld_preload.enabled: true
  file_locks.ld_preload.agents: [opencode, aider]
```

### Advanced Lock Features

#### Read/Write Lock Distinction

Not all file access requires exclusive locks. NEEDLE distinguishes:

| Operation | Lock Type | Concurrency |
|-----------|-----------|-------------|
| Read, Glob, Grep, cat | Shared (read) | Multiple simultaneous |
| Edit, Write, rm | Exclusive (write) | One at a time |

**Lock structure with type:**
```json
{
  "bead": "nd-2ov",
  "worker": "alpha",
  "path": "/src/cli/run.sh",
  "type": "write",
  "ts": 1709337600,
  "readers": []
}
```

**Shared lock structure (multiple readers):**
```json
{
  "path": "/src/cli/run.sh",
  "type": "read",
  "readers": [
    {"bead": "nd-2ov", "worker": "alpha", "ts": 1709337600},
    {"bead": "bd-muv", "worker": "bravo", "ts": 1709337605}
  ]
}
```

**Lock compatibility matrix:**

| Held \ Requested | Read | Write |
|------------------|------|-------|
| None | ✅ Grant | ✅ Grant |
| Read | ✅ Grant | ❌ Block (wait for readers) |
| Write | ❌ Block | ❌ Block |

**API:**
```bash
checkout_file "$path" --read "$bead_id"   # Shared lock
checkout_file "$path" --write "$bead_id"  # Exclusive lock
```

**Interception determines lock type automatically:**
- `open()` with `O_RDONLY` → read lock
- `open()` with `O_WRONLY|O_RDWR|O_CREAT|O_TRUNC` → write lock

#### Intent Declaration (Proactive Reservation)

Beads can optionally declare files they intend to modify upfront. This enables conflict detection before agent execution begins.

**Declaration sources (checked in order):**
1. Explicit `files:` field in bead metadata
2. File paths parsed from bead description
3. Historical patterns (beads with similar titles → similar files)

**Bead with intent declaration:**
```bash
br create "Refactor authentication flow" \
  --files="src/auth/login.ts,src/auth/logout.ts,src/auth/types.ts"
```

**Automatic extraction from description:**
```bash
# Bead description: "Fix bug in src/cli/run.sh where parse_args fails"
# NEEDLE extracts: src/cli/run.sh

extract_files_from_description() {
    local desc="$1"
    # Match common path patterns
    echo "$desc" | grep -oE '[a-zA-Z0-9_./-]+\.(ts|js|py|sh|rs|go|rb|yaml|json|md)' | sort -u
}
```

**Claim with pre-reservation:**
```bash
# On bead claim, attempt to reserve declared files
claim_with_intent() {
    local bead_id="$1"
    local files=$(br show "$bead_id" --json | jq -r '.files // empty')

    if [ -n "$files" ]; then
        for file in $files; do
            if ! checkout_file "$file" --write "$bead_id"; then
                blocking=$(check_file "$file" | jq -r '.bead')
                br dep add "$bead_id" "$blocking"
                br update "$bead_id" --status open  # Release claim
                return 1  # Find different bead
            fi
        done
    fi
    return 0  # All files reserved, proceed
}
```

**Benefits:**
- Zero wasted agent execution on conflicts
- Conflicts become dependencies before work begins
- Better scheduling (known file sets enable parallel planning)

#### Optimistic Locking with 3-Way Merge

Instead of blocking on conflict, allow concurrent edits and merge at completion:

**Workflow:**
```
Bead A claims file.ts         Bead B claims file.ts
    ↓                             ↓
Snapshot: file.ts.A.base      Snapshot: file.ts.B.base
    ↓                             ↓
Agent A edits file.ts         Agent B edits file.ts
    ↓                             ↓
A completes first             B completes second
    ↓                             ↓
Commits file.ts               Attempts merge:
                              git merge-file file.ts file.ts.B.base file.ts.A.committed
                                  ↓
                              ┌─ Clean merge ─┐    ┌─ Conflict ─┐
                              │ Auto-merged!  │    │ Block, add │
                              │ Both succeed  │    │ dependency │
                              └───────────────┘    └────────────┘
```

**Implementation:**
```bash
# Before agent execution - snapshot base version
prepare_optimistic_edit() {
    local file="$1"
    local bead_id="$2"
    local snapshot="/dev/shm/needle-snapshots/${bead_id}/$(echo "$file" | md5sum | cut -d' ' -f1)"

    mkdir -p "$(dirname "$snapshot")"
    cp "$file" "$snapshot.base" 2>/dev/null || touch "$snapshot.base"
    echo "$file" >> "$snapshot.files"
}

# After agent execution - attempt merge if concurrent edits occurred
reconcile_optimistic_edits() {
    local bead_id="$1"
    local snapshot_dir="/dev/shm/needle-snapshots/${bead_id}"

    [ -d "$snapshot_dir" ] || return 0

    while read -r file; do
        local hash=$(echo "$file" | md5sum | cut -d' ' -f1)
        local base="$snapshot_dir/$hash.base"

        # Check if file was modified by another bead since we started
        local current_in_repo=$(git show HEAD:"$file" 2>/dev/null)
        local our_base=$(cat "$base")

        if [ "$current_in_repo" != "$our_base" ]; then
            # Concurrent modification detected - attempt merge
            local theirs=$(mktemp)
            git show HEAD:"$file" > "$theirs"

            if git merge-file "$file" "$base" "$theirs" 2>/dev/null; then
                log_info "Auto-merged concurrent edits to $file"
                emit_event "file.merge.success" "{\"bead\":\"$bead_id\",\"path\":\"$file\"}"
            else
                log_error "Merge conflict in $file"
                emit_event "file.merge.conflict" "{\"bead\":\"$bead_id\",\"path\":\"$file\"}"
                # Restore our version, add dependency, re-queue
                git checkout HEAD -- "$file"
                return 1
            fi
            rm "$theirs"
        fi
    done < "$snapshot_dir/files"

    rm -rf "$snapshot_dir"
    return 0
}
```

**Configuration:**
```yaml
file_locks:
  strategy: optimistic  # or: pessimistic (default)
  merge:
    enabled: true
    tool: git-merge-file  # or: diff3, custom
    on_conflict: block    # or: keep_ours, keep_theirs
```

#### Priority-Based Lock Queuing

When a high-priority bead needs a file locked by a lower-priority bead:

**Queue structure:**
```json
{
  "path": "/src/cli/run.sh",
  "holder": {"bead": "nd-low", "priority": 2, "worker": "alpha"},
  "queue": [
    {"bead": "nd-high", "priority": 0, "worker": "bravo", "ts": 1709337700}
  ]
}
```

**Priority bump mechanism:**
```bash
request_lock_with_priority() {
    local path="$1"
    local bead_id="$2"
    local priority="$3"

    local lock_info=$(check_file "$path")
    if [ -z "$lock_info" ]; then
        checkout_file "$path" --write "$bead_id"
        return 0
    fi

    local holder_priority=$(echo "$lock_info" | jq -r '.holder.priority')
    local holder_bead=$(echo "$lock_info" | jq -r '.holder.bead')

    if [ "$priority" -lt "$holder_priority" ]; then
        # We're higher priority - add to queue and signal holder
        add_to_queue "$path" "$bead_id" "$priority"

        emit_event "lock.priority_bump" "{
            \"path\": \"$path\",
            \"waiting_bead\": \"$bead_id\",
            \"waiting_priority\": $priority,
            \"holder_bead\": \"$holder_bead\",
            \"holder_priority\": $holder_priority
        }"

        # Holder's worker receives signal to expedite or yield
        signal_worker "$holder_bead" "PRIORITY_BUMP"
    fi

    # Add dependency and find other work
    br dep add "$bead_id" "$holder_bead"
    return 1
}
```

**Worker response to priority bump:**
```bash
handle_priority_bump() {
    log_warn "Priority bump received - higher priority bead waiting"
    # Options:
    # 1. Complete current work faster (reduce exploration)
    # 2. Checkpoint and yield (if supported)
    # 3. Continue but emit ETA for waiting bead
}
```

#### Lock Lease Renewal

Locks require periodic heartbeat to remain valid. Stale locks auto-release:

**Heartbeat includes active locks:**
```json
{
  "worker": "alpha",
  "ts": 1709337600,
  "bead": "nd-2ov",
  "locks": [
    "/src/cli/run.sh",
    "/src/cli/init.sh"
  ]
}
```

**Lease renewal daemon:**
```bash
# Runs in worker's heartbeat loop
renew_lock_leases() {
    local bead_id="$1"

    for lock in /dev/shm/needle-locks/*.lock; do
        local holder=$(jq -r '.bead' "$lock/info" 2>/dev/null)
        if [ "$holder" = "$bead_id" ]; then
            # Renew lease
            jq ".ts = $(date +%s)" "$lock/info" > "$lock/info.tmp"
            mv "$lock/info.tmp" "$lock/info"
        fi
    done
}

# Lease expiry check (runs periodically)
expire_stale_leases() {
    local now=$(date +%s)
    local lease_timeout=60  # seconds

    for lock in /dev/shm/needle-locks/*.lock; do
        [ -d "$lock" ] || continue
        local ts=$(jq -r '.ts' "$lock/info" 2>/dev/null)
        local bead=$(jq -r '.bead' "$lock/info" 2>/dev/null)

        if (( now - ts > lease_timeout )); then
            log_warn "Expiring stale lock: $lock (held by $bead, age=$((now - ts))s)"
            emit_event "lock.expired" "{\"path\":\"$lock\",\"bead\":\"$bead\",\"age_s\":$((now - ts))}"
            rm -rf "$lock"
        fi
    done
}
```

**Configuration:**
```yaml
file_locks:
  lease:
    duration: 60s       # Lock valid for this long without renewal
    renewal_interval: 15s  # Renew every N seconds
    grace_period: 10s   # Extra time before forceful release
```

#### Hot File Detection and Split Recommendations

NEEDLE tracks file contention and recommends architectural changes:

**Contention tracking:**
```bash
# Updated on every conflict
track_file_contention() {
    local path="$1"
    local contention_file="$HOME/.needle/state/metrics/file_contention.json"

    # Increment conflict count for this file
    jq --arg path "$path" '
        .files[$path].conflicts += 1 |
        .files[$path].last_conflict = now
    ' "$contention_file" > "$contention_file.tmp"
    mv "$contention_file.tmp" "$contention_file"
}
```

**Analysis and recommendations:**
```bash
$ needle analyze hot-files

📊 File Contention Analysis (last 30 days)

🔥 HOT FILES (>10 conflicts):

1. /src/cli/run.sh (47 conflicts)
   ├─ Edited by 12 different beads
   ├─ Functions involved:
   │   ├─ parse_args (18 conflicts)
   │   ├─ main (15 conflicts)
   │   └─ validate_config (14 conflicts)
   └─ Recommendation: SPLIT
      Suggested structure:
        src/cli/run/
        ├─ args.sh      (parse_args, validate_args)
        ├─ main.sh      (main, run_loop)
        └─ config.sh    (validate_config, load_config)

2. /src/agent/dispatch.sh (23 conflicts)
   ├─ Edited by 8 different beads
   └─ Recommendation: SPLIT by agent type
      Suggested structure:
        src/agent/
        ├─ dispatch.sh  (common logic)
        ├─ claude.sh    (Claude-specific)
        ├─ opencode.sh  (OpenCode-specific)
        └─ aider.sh     (Aider-specific)

3. /README.md (12 conflicts)
   └─ Recommendation: Consider section-based locking
      or documentation-specific workflow

💡 Tip: Run `needle refactor suggest /src/cli/run.sh` for
   detailed split plan with migration steps.
```

**Auto-generated refactoring bead:**
```bash
$ needle analyze hot-files --create-beads

✓ Created nd-split-1: Refactor /src/cli/run.sh into modular components
  Priority: P2
  Description: Split hot file to reduce contention (47 conflicts in 30 days)

✓ Created nd-split-2: Refactor /src/agent/dispatch.sh by agent type
  Priority: P2
  Description: Split hot file to reduce contention (23 conflicts in 30 days)
```

---

## Worker Heartbeat & Auto-Recovery

Workers emit periodic heartbeats. Stuck workers are detected and recovered automatically.

### Heartbeat Protocol

Workers write heartbeat files to `~/.needle/state/heartbeats/`:

```
~/.needle/state/heartbeats/
├── needle-claude-anthropic-sonnet-alpha.json
├── needle-claude-anthropic-sonnet-bravo.json
└── needle-opencode-alibaba-qwen-charlie.json
```

**Heartbeat file format:**
```json
{
  "worker": "needle-claude-anthropic-sonnet-alpha",
  "pid": 12345,
  "started": "2026-03-01T10:00:00Z",
  "last_heartbeat": "2026-03-01T10:30:00Z",
  "status": "executing",
  "current_bead": "bd-123",
  "bead_started": "2026-03-01T10:25:00Z",
  "strand": 1,
  "workspace": "/home/coder/project",
  "agent": "claude-anthropic-sonnet"
}
```

### Heartbeat Configuration

```yaml
# ~/.needle/config.yaml
heartbeat:
  # How often workers emit heartbeats
  interval: 30s

  # Worker considered stuck if no heartbeat for this duration
  timeout: 120s

  # Bead considered stuck if same bead for this duration
  bead_timeout: 600s

  # Recovery behavior
  recovery:
    enabled: true

    # What to do with stuck workers
    action: restart       # restart | alert | kill

    # Release the bead back to queue (remove claim)
    release_bead: true

    # Create alert bead when recovering
    create_alert: true

    # Max recovery attempts before giving up
    max_attempts: 3

    # Cooldown between recovery attempts
    cooldown: 60s
```

### Stuck Detection

NEEDLE runs a background watchdog that monitors heartbeats:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                       HEARTBEAT WATCHDOG                                    │
│                                                                             │
│   Every 30 seconds:                                                         │
│                                                                             │
│   1. Scan ~/.needle/state/heartbeats/*.json                                │
│                                                                             │
│   2. For each worker:                                                       │
│      ├─ last_heartbeat > 120s ago?                                         │
│      │    → Worker stuck (no heartbeat)                                    │
│      │                                                                      │
│      ├─ bead_started > 600s ago?                                           │
│      │    → Bead stuck (execution timeout)                                 │
│      │                                                                      │
│      └─ Process still alive? (check PID)                                   │
│           → If dead: Clean up orphaned state                               │
│                                                                             │
│   3. Recovery action:                                                       │
│      ├─ Release bead (br update bd-xxx --release)                          │
│      ├─ Kill stuck process (if still running)                              │
│      ├─ Respawn worker (if action=restart)                                 │
│      └─ Create alert bead                                                  │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Recovery Flow

```
Worker alpha: last heartbeat 150s ago (timeout: 120s)
  │
  ├─ 1. Detect stuck
  │     Log: "Worker alpha stuck, no heartbeat for 150s"
  │
  ├─ 2. Release bead
  │     br update bd-123 --release --actor watchdog
  │     Log: "Released bd-123 back to queue"
  │
  ├─ 3. Kill worker (if still running)
  │     kill -9 $PID
  │     Log: "Killed stuck worker process 12345"
  │
  ├─ 4. Clean up state
  │     rm ~/.needle/state/heartbeats/...-alpha.json
  │
  ├─ 5. Create alert bead
  │     br create "Worker alpha stuck on bd-123" --priority 1 --label alert
  │
  └─ 6. Respawn (if action=restart)
        needle run --agent=claude-anthropic-sonnet --id=alpha
        Log: "Respawned worker alpha"
```

### Watchdog Commands

```bash
# Check heartbeat status of all workers
needle heartbeat status

# Output:
# Worker                                   Status    Last Beat   Current Bead
# needle-claude-anthropic-sonnet-alpha     healthy   5s ago      bd-123
# needle-claude-anthropic-sonnet-bravo     healthy   12s ago     (idle)
# needle-opencode-alibaba-qwen-charlie     STUCK     135s ago    bd-456

# Manually trigger recovery for stuck worker
needle heartbeat recover alpha

# Disable auto-recovery temporarily
needle heartbeat pause

# Resume auto-recovery
needle heartbeat resume
```

### Telemetry Events

```jsonl
{"ts":"...","event":"heartbeat.emitted","session":"...","worker":{...},"data":{"status":"executing","current_bead":"bd-123"}}
{"ts":"...","event":"heartbeat.stuck_detected","session":"...","data":{"worker":"alpha","last_heartbeat_ago_s":150,"threshold_s":120}}
{"ts":"...","event":"heartbeat.bead_released","session":"...","data":{"worker":"alpha","bead_id":"bd-123","reason":"stuck_timeout"}}
{"ts":"...","event":"heartbeat.worker_killed","session":"...","data":{"worker":"alpha","pid":12345,"signal":"SIGKILL"}}
{"ts":"...","event":"heartbeat.worker_respawned","session":"...","data":{"worker":"alpha","new_pid":12346}}
{"ts":"...","event":"heartbeat.recovery_failed","session":"...","data":{"worker":"alpha","attempt":3,"max_attempts":3,"giving_up":true}}
```

### Graceful Shutdown Integration

Heartbeat integrates with graceful shutdown:

```
SIGTERM received
  │
  ├─ Update heartbeat: status = "draining"
  │
  ├─ Stop accepting new beads
  │
  ├─ Continue current bead (with drain_timeout)
  │
  ├─ Update heartbeat: status = "stopped"
  │
  └─ Remove heartbeat file on clean exit
```

Watchdog recognizes `status: "draining"` and won't trigger recovery for draining workers.

---

## Agent Adapter System

NEEDLE's primary purpose is to wrap different headless coding CLIs behind a unified interface. The adapter system is central to this design.

### Headless Invocation Requirements

All agents MUST be invoked with:
1. **Headless mode** - No interactive prompts, execute and exit
2. **Permission bypass** - Auto-approve all file edits, commands, etc.
3. **Structured output** - JSON or parseable output for telemetry extraction

| Runner | Headless Flag | Permission Bypass | Output Format |
|--------|---------------|-------------------|---------------|
| **claude** | `--print` | `--dangerously-skip-permissions` | stdout (text + JSON stats) |
| **opencode** | `--headless` | `--yes` | stdout + exit code |
| **codex** | `--quiet` | `--approval-mode full-auto` | stdout |
| **aider** | (implicit with `--message`) | `--yes` | stdout |

### Supported Agents

| Runner | Providers | Headless CLI | Input Method | Cost Model |
|--------|-----------|--------------|--------------|------------|
| **claude** | anthropic | `claude --print --dangerously-skip-permissions` | stdin | Pay-per-token |
| **opencode** | alibaba, zai, openai, ollama | `opencode --headless --yes` | file | Varies |
| **codex** | openai | `codex --quiet --approval-mode full-auto` | stdin | Pay-per-token |
| **aider** | openai, anthropic, ollama | `aider --yes --message` | args | Varies |
| **custom** | any | configurable | configurable | configurable |

**Common Provider + Model Combinations:**

| Agent Name | Runner | Provider | Model | Description |
|------------|--------|----------|-------|-------------|
| `claude-anthropic-sonnet` | claude | anthropic | sonnet | Claude Sonnet 4.5 |
| `claude-anthropic-opus` | claude | anthropic | opus | Claude Opus 4.5 |
| `opencode-alibaba-qwen` | opencode | alibaba | qwen | Qwen Coder Plus |
| `opencode-zai-glm5` | opencode | zai | glm5 | ZhipuAI GLM-5 |
| `opencode-ollama-deepseek` | opencode | ollama | deepseek | Local DeepSeek |
| `codex-openai-gpt4` | codex | openai | gpt4 | OpenAI GPT-4 |
| `aider-ollama-deepseek` | aider | ollama | deepseek | Aider + local DeepSeek |

### Adapter Configuration

Each agent has a YAML configuration file with runner, provider, model metadata and a bash `invoke` template:

```yaml
# agents/claude-anthropic-sonnet.yaml
runner: claude
provider: anthropic
model: sonnet
description: "Claude Code with Sonnet 4.5"

invoke: |
  cd ${WORKSPACE} && \
  ANTHROPIC_MODEL=claude-sonnet-4-5-20250929 \
  claude --print \
         --dangerously-skip-permissions \
         --output-format json \
         --verbose \
  <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT

cost:
  type: pay-per-token
  input_per_1k: 0.003
  output_per_1k: 0.015

timeout: 600
```

```yaml
# agents/claude-anthropic-opus.yaml
runner: claude
provider: anthropic
model: opus
description: "Claude Code with Opus 4.5"

invoke: |
  cd ${WORKSPACE} && \
  ANTHROPIC_MODEL=claude-opus-4-5-20251101 \
  claude --print \
         --dangerously-skip-permissions \
         --output-format json \
         --verbose \
  <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT

cost:
  type: pay-per-token
  input_per_1k: 0.015
  output_per_1k: 0.075

timeout: 900
```

```yaml
# agents/opencode-alibaba-qwen.yaml
runner: opencode
provider: alibaba
model: qwen
description: "OpenCode with Alibaba Qwen"

invoke: |
  cd ${WORKSPACE} && \
  OPENCODE_MODEL=qwen-coder-plus \
  opencode --headless \
           --yes \
           --json-output \
           --prompt-file ${PROMPT_FILE}

cost:
  type: pay-per-token
  input_per_1k: 0.0005
  output_per_1k: 0.001

timeout: 900
```

```yaml
# agents/opencode-zai-glm5.yaml
runner: opencode
provider: zai
model: glm5
description: "OpenCode with ZhipuAI GLM-5"

invoke: |
  cd ${WORKSPACE} && \
  OPENCODE_MODEL=glm-5 \
  OPENCODE_API_BASE=https://open.bigmodel.cn/api/paas/v4 \
  opencode --headless \
           --yes \
           --json-output \
           --prompt-file ${PROMPT_FILE}

cost:
  type: pay-per-token
  input_per_1k: 0.001
  output_per_1k: 0.002

timeout: 900
```

```yaml
# agents/codex-openai-gpt4.yaml
runner: codex
provider: openai
model: gpt4
description: "OpenAI Codex CLI with GPT-4"

invoke: |
  cd ${WORKSPACE} && \
  codex --quiet \
        --approval-mode full-auto \
        --json \
  <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT

cost:
  type: pay-per-token
  input_per_1k: 0.01
  output_per_1k: 0.03

timeout: 600
```

```yaml
# agents/aider-ollama-deepseek.yaml
runner: aider
provider: ollama
model: deepseek
description: "Aider with local Ollama DeepSeek"

invoke: |
  cd ${WORKSPACE} && \
  aider --yes \
        --no-git \
        --no-pretty \
        --model ollama/deepseek-coder \
        --message '${PROMPT}'

cost:
  type: unlimited

timeout: 600
```

```yaml
# agents/custom-example.yaml
runner: custom
provider: local
model: mymodel
description: "Template for custom agents"

# Custom agents MUST support:
# 1. Headless execution (no interactive prompts)
# 2. Auto-approve all actions (no permission dialogs)
# 3. Exit when complete (no REPL mode)
# 4. Preferably JSON output for telemetry parsing

invoke: |
  cd ${WORKSPACE} && \
  CUSTOM_VAR=value \
  /path/to/my-agent \
    --headless \
    --auto-approve \
    --json-output \
    --execute <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT

cost:
  type: unlimited

timeout: 300
```

### Invocation Model

NEEDLE invokes headless CLIs via **bash** with the prompt injected into the command. Each agent config defines an `invoke` template that NEEDLE renders with the prompt.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         NEEDLE INVOCATION FLOW                              │
│                                                                             │
│   1. NEEDLE selects bead and builds prompt                                  │
│   2. NEEDLE loads agent config (YAML)                                       │
│   3. NEEDLE renders invoke template with prompt                             │
│   4. NEEDLE executes via: bash -c "<rendered command>"                      │
│   5. Agent executes, writes to stdout, exits                                │
│   6. NEEDLE captures output and exit code                                   │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Example invocation:**
```bash
# NEEDLE renders this from the agent config template:
bash -c 'cd /home/coder/project && \
  claude --print \
         --dangerously-skip-permissions \
         --output-format json \
         --verbose \
  <<'\''NEEDLE_PROMPT'\''
Fix the authentication bug in src/auth.py.
The login function should validate the JWT token before proceeding.
NEEDLE_PROMPT'
```

### Invoke Templates

Each agent config has an `invoke` field with a bash command template. NEEDLE substitutes:

| Variable | Description |
|----------|-------------|
| `${PROMPT}` | The full prompt text (escaped for bash) |
| `${WORKSPACE}` | Working directory path |
| `${BEAD_ID}` | Current bead identifier |
| `${PROMPT_FILE}` | Path to temp file containing prompt (if needed) |

**Required Flags for All Agents:**

| Purpose | Claude | OpenCode | Codex | Aider |
|---------|--------|----------|-------|-------|
| Headless mode | `--print` | `--headless` | `--quiet` | (implicit) |
| Skip permissions | `--dangerously-skip-permissions` | `--yes` | `--approval-mode full-auto` | `--yes` |
| JSON output | `--output-format json` | `--json-output` | `--json` | `--no-pretty` |
| Verbose logging | `--verbose` | (default) | (default) | (default) |

**Template Examples:**

```yaml
# Claude Code - heredoc style with full flags
invoke: |
  cd ${WORKSPACE} && \
  claude --print \
         --dangerously-skip-permissions \
         --output-format json \
         --verbose \
  <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT

# OpenCode - prompt file style with full flags
invoke: |
  cd ${WORKSPACE} && \
  opencode --headless \
           --yes \
           --json-output \
           --prompt-file ${PROMPT_FILE}

# Codex - stdin style with full flags
invoke: |
  cd ${WORKSPACE} && \
  codex --quiet \
        --approval-mode full-auto \
        --json \
  <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT

# Aider - argument style with full flags
invoke: |
  cd ${WORKSPACE} && \
  aider --yes \
        --no-git \
        --no-pretty \
        --message '${PROMPT}'

# Custom - must include headless + auto-approve flags
invoke: |
  cd ${WORKSPACE} && \
  MY_VAR=value \
  /path/to/agent \
    --headless \
    --auto-approve \
    --json-output \
  <<'EOF'
  ${PROMPT}
  EOF
```

### Adding a New Agent

1. Create `agents/{runner}-{provider}-{model}.yaml` with the configuration
2. Define the `invoke` template with bash command
3. **Ensure headless mode** - agent must execute and exit without prompts
4. **Ensure permission bypass** - agent must auto-approve all file edits, commands
5. **Prefer JSON output** - enables NEEDLE to extract tokens, cost, duration
6. Test with: `needle test-agent {runner}-{provider}-{model} --prompt "Hello world"`
7. Use with: `needle run --agent={runner}-{provider}-{model} --workspace=/path`

No code changes required—just YAML configuration with a bash invoke template.

---

### Logging & Telemetry

NEEDLE outputs structured events as **JSON Lines (JSONL)** to enable parsing, filtering, and integration with observability tools.

#### Output Destinations

```yaml
# ~/.needle/config.yaml
logging:
  # Write to stdout (visible in tmux session)
  stdout: true

  # Write to file (one per worker session)
  file: ~/.needle/logs/{session}.jsonl

  # Log level: debug, info, warn, error
  level: info
```

#### Event Schema

All events share a common envelope:

```json
{
  "ts": "2026-03-01T10:00:00.123Z",
  "event": "bead.claimed",
  "session": "needle-claude-anthropic-sonnet-alpha",
  "worker": {
    "runner": "claude",
    "provider": "anthropic",
    "model": "sonnet",
    "identifier": "alpha"
  },
  "data": { ... }
}
```

#### Event Types

**Strand Events:**

The needle follows strands 1→6 to find work. These events track which strand is active:

```jsonl
{"ts":"...","event":"strand.started","session":"...","worker":{...},"data":{"strand":1,"name":"pluck","description":"Plucking beads from configured workspaces"}}
{"ts":"...","event":"strand.completed","session":"...","worker":{...},"data":{"strand":1,"name":"pluck","result":"work_found","beads_processed":3}}
{"ts":"...","event":"strand.skipped","session":"...","worker":{...},"data":{"strand":4,"name":"weave","reason":"disabled_in_config"}}
{"ts":"...","event":"strand.fallthrough","session":"...","worker":{...},"data":{"from":1,"to":2,"from_name":"pluck","to_name":"explore","reason":"no_work_found"}}
```

| Strand | Name | Invokes Agent | Description |
|--------|------|---------------|-------------|
| `1` | `pluck` | Yes | Pluck beads from configured workspaces |
| `2` | `explore` | No | Look for work in other workspaces |
| `3` | `mend` | No | Maintenance and cleanup |
| `4` | `weave` | Yes | Create beads from documentation (opt-in) |
| `5` | `unravel` | Yes | Create alternatives for HUMAN beads (opt-in) |
| `6` | `pulse` | Yes | Codebase health monitoring (opt-in) |
| `7` | `knot` | No | Alert human when stuck |

**Lifecycle Events:**

```jsonl
{"ts":"...","event":"worker.started","session":"...","worker":{...},"data":{"workspace":"/home/coder/project","config_hash":"abc123"}}
{"ts":"...","event":"worker.idle","session":"...","worker":{...},"data":{"idle_seconds":30,"consecutive_empty":3,"last_strand":"mend"}}
{"ts":"...","event":"worker.stopped","session":"...","worker":{...},"data":{"reason":"idle_timeout","beads_completed":5}}
```

**Bead Events:**

All bead events include `strand` to show which strand the needle was following:

```jsonl
{"ts":"...","event":"bead.claimed","session":"...","worker":{...},"data":{"bead_id":"bd-abc123","strand":1,"strand_name":"pluck","bead_priority":1,"title":"Fix auth bug"}}
{"ts":"...","event":"bead.prompt_built","session":"...","worker":{...},"data":{"bead_id":"bd-abc123","strand":1,"prompt_tokens":1200}}
{"ts":"...","event":"bead.agent_started","session":"...","worker":{...},"data":{"bead_id":"bd-abc123","strand":1,"agent":"claude-anthropic-sonnet"}}
{"ts":"...","event":"bead.agent_completed","session":"...","worker":{...},"data":{"bead_id":"bd-abc123","strand":1,"exit_code":0,"duration_ms":45000}}
{"ts":"...","event":"bead.completed","session":"...","worker":{...},"data":{"bead_id":"bd-abc123","strand":1,"status":"closed","duration_ms":47000}}
{"ts":"...","event":"bead.failed","session":"...","worker":{...},"data":{"bead_id":"bd-abc123","strand":1,"error":"agent_timeout","duration_ms":600000}}
```

**Effort/Cost Events:**

```jsonl
{"ts":"...","event":"effort.recorded","session":"...","worker":{...},"data":{"bead_id":"bd-abc123","tokens":{"input":5000,"output":2500},"cost_usd":0.0525,"duration_ms":150000}}
{"ts":"...","event":"budget.warning","session":"...","worker":{...},"data":{"daily_spend_usd":40.0,"daily_limit_usd":50.0,"threshold":0.8}}
{"ts":"...","event":"budget.exceeded","session":"...","worker":{...},"data":{"daily_spend_usd":50.5,"daily_limit_usd":50.0}}
```

**Rate Limit Events:**

```jsonl
{"ts":"...","event":"ratelimit.waiting","session":"...","worker":{...},"data":{"provider":"anthropic","wait_seconds":15,"reason":"requests_per_minute"}}
{"ts":"...","event":"ratelimit.concurrency_blocked","session":"...","worker":{...},"data":{"agent":"claude-anthropic-opus","current":2,"max":2}}
```

**Error Events:**

```jsonl
{"ts":"...","event":"error.claim_failed","session":"...","worker":{...},"data":{"bead_id":"bd-abc123","reason":"already_assigned","owner":"needle-claude-anthropic-sonnet-bravo"}}
{"ts":"...","event":"error.agent_crash","session":"...","worker":{...},"data":{"bead_id":"bd-abc123","exit_code":137,"signal":"SIGKILL"}}
{"ts":"...","event":"error.workspace_unavailable","session":"...","worker":{...},"data":{"workspace":"/home/coder/project","reason":"not_found"}}
```

#### Parsing Events

Events can be easily parsed with standard tools:

```bash
# Watch live events
tail -f ~/.needle/logs/needle-claude-anthropic-sonnet-alpha.jsonl | jq .

# Filter by event type
cat ~/.needle/logs/*.jsonl | jq 'select(.event == "bead.completed")'

# Calculate total cost
cat ~/.needle/logs/*.jsonl | jq -s '[.[] | select(.event == "effort.recorded") | .data.cost_usd] | add'

# Count beads by status
cat ~/.needle/logs/*.jsonl | jq -s 'group_by(.event) | map({event: .[0].event, count: length})'

# Get failed beads
cat ~/.needle/logs/*.jsonl | jq 'select(.event == "bead.failed") | {bead: .data.bead_id, error: .data.error}'

# Filter by strand
cat ~/.needle/logs/*.jsonl | jq 'select(.data.strand == 1)'

# Count work done per strand
cat ~/.needle/logs/*.jsonl | jq -s '[.[] | select(.event == "bead.completed")] | group_by(.data.strand) | map({strand: .[0].data.strand, name: .[0].data.strand_name, count: length})'

# Track strand fallthrough patterns (needle following different strands)
cat ~/.needle/logs/*.jsonl | jq 'select(.event == "strand.fallthrough") | {from: .data.from_name, to: .data.to_name, reason: .data.reason}'

# See which strands are being used
cat ~/.needle/logs/*.jsonl | jq 'select(.event | startswith("strand.")) | {event, strand: .data.strand, name: .data.name}'
```

#### Integration with Observability Tools

The JSONL format integrates with:

- **Log aggregators**: Loki, Elasticsearch, Splunk (ingest JSONL directly)
- **Metrics**: Parse events to emit Prometheus metrics
- **Dashboards**: Grafana queries on structured fields
- **Alerting**: Alert on `budget.exceeded`, `error.*` events

---

## Desirable Features

### Must Have (Phase 1)

| Feature | Description |
|---------|-------------|
| **Agent adapters** | YAML-configurable adapters for Claude Code, OpenCode, Codex, Aider |
| **Bash invocation** | Render invoke template, inject prompt, execute via `bash -c` |
| Self-invoking tmux | `needle run` creates tmux session automatically |
| Configurable naming | Pattern with placeholders (default: `needle-{runner}-{provider}-{model}-{identifier}`) |
| Strand 1 (pluck) | Select, claim, execute beads from workspaces |
| Atomic claims | Via `br update --claim` (SQLite transactions) |
| Structured event logging | JSONL events to stdout and file (lifecycle, bead, effort, errors) |
| Worker management | list, attach, stop, filter by runner/provider/model |
| `needle test-agent` | Test an agent adapter without running full loop |

### Should Have (Phase 2)

| Feature | Description |
|---------|-------------|
| Multi-worker launch | `--count=N` spawns multiple workers |
| Strand 2 (explore) | Look for work in other workspaces, auto-scaling |
| Strand 3 (mend) | Maintenance - clean orphaned claims, health checks |
| Token tracking | Extract token counts from agent output |
| Cost calculation | Track spend per bead, per day |
| Budget enforcement | Stop/warn when approaching limits |
| **Hook system** | User-defined bash scripts at lifecycle events (pre-claim, post-execute, etc.) |
| **Worker heartbeat** | Periodic heartbeat emission for stuck detection |
| **Auto-recovery** | Watchdog detects stuck workers, releases beads, respawns |

### Nice to Have (Phase 3)

| Feature | Description |
|---------|-------------|
| Strand 4 (weave) | Create beads from documentation gaps |
| Strand 5 (unravel) | Create alternatives for HUMAN beads |
| Strand 6 (pulse) | Codebase health monitoring, auto-generate beads |
| Strand 7 (knot) | Alert human when stuck |
| Bead mitosis | Automatic bead decomposition for complex tasks |
| Workspace discovery | Auto-discover beads workspaces |
| Dashboard | Real-time worker status UI |

---

## File Structure

### Release Artifact

NEEDLE is distributed as a **single self-contained bash script**:

```
~/.local/bin/needle    # Single executable (~50-100KB)
```

The script embeds all functionality and agent configurations. On first run, it creates:

```
~/.needle/
├── config.yaml              # User configuration (created from defaults)
├── agents/                  # Custom agent adapters (user-defined)
│   └── my-custom-agent.yaml
├── logs/                    # Structured event logs (JSONL per session)
│   ├── needle-claude-anthropic-sonnet-alpha.jsonl
│   ├── needle-claude-anthropic-sonnet-bravo.jsonl
│   └── needle-opencode-alibaba-qwen-charlie.jsonl
├── state/                   # Runtime state
│   ├── workers.json         # Active worker registry
│   ├── rate_limits/         # Rate limit tracking per provider
│   ├── heartbeats/          # Worker heartbeat files (stuck detection)
│   │   ├── needle-...-alpha.json
│   │   └── needle-...-bravo.json
│   └── pulse/               # Pulse strand state (codebase health)
│       ├── last_scan.json   # Timestamp and results of last scan
│       └── seen_issues.json # Issues already converted to beads (dedup)
├── hooks/                   # User-defined lifecycle hooks
│   ├── pre-claim.sh
│   ├── post-execute.sh
│   └── pre-complete.sh
└── cache/                   # Downloaded binaries and update artifacts
    ├── br                   # Cached dependency binaries
    ├── jq
    ├── yq
    ├── version_check        # Cached latest version (24h TTL)
    ├── needle-1.2.0.bak     # Backup of previous version (for rollback)
    └── needle-1.3.0.new     # Downloaded update (before swap)
```

### Development Structure

For development and testing, the repository uses a modular structure:

```
NEEDLE/
├── README.md
├── install.sh                  # One-liner installer script
├── docs/
│   └── plan.md
├── bin/
│   └── needle                  # Main CLI entry point (sources src/)
├── src/
│   ├── cli/
│   │   ├── init.sh             # needle init (interactive onboarding)
│   │   ├── run.sh              # needle run
│   │   ├── list.sh             # needle list
│   │   ├── attach.sh           # needle attach
│   │   ├── logs.sh             # needle logs
│   │   ├── stop.sh             # needle stop
│   │   ├── setup.sh            # needle setup (bootstrap)
│   │   ├── agents.sh           # needle agents (list/scan)
│   │   └── version.sh          # needle version
│   ├── onboarding/
│   │   ├── welcome.sh          # ASCII banner and welcome message
│   │   ├── detect_agents.sh    # Scan PATH for coding CLIs
│   │   ├── workspace_setup.sh  # Prompt for workspace, validate beads
│   │   └── create_config.sh    # Generate default config.yaml
│   ├── bootstrap/
│   │   ├── check.sh            # Check if dependencies installed
│   │   ├── install.sh          # Install missing dependencies
│   │   ├── detect_os.sh        # Detect OS and package manager
│   │   └── paths.sh            # PATH management
│   ├── runner/
│   │   ├── loop.sh             # Main worker loop
│   │   ├── tmux.sh             # tmux session management
│   │   └── naming.sh           # NATO alphabet naming
│   ├── strands/
│   │   ├── engine.sh           # Strand dispatcher (follows strands 1→7)
│   │   ├── pluck.sh            # Strand 1: Pluck beads from workspaces
│   │   ├── explore.sh          # Strand 2: Look for work elsewhere
│   │   ├── mend.sh             # Strand 3: Maintenance & cleanup
│   │   ├── weave.sh            # Strand 4: Create beads from doc gaps
│   │   ├── unravel.sh          # Strand 5: Create HUMAN alternatives
│   │   ├── pulse.sh            # Strand 6: Codebase health monitoring
│   │   └── knot.sh             # Strand 7: Alert human when stuck
│   ├── bead/
│   │   ├── claim.sh            # Claim with retry (wraps br update --claim)
│   │   ├── select.sh           # Weighted selection from br ready
│   │   ├── mitosis.sh          # Complexity analysis and decomposition
│   │   └── prompt.sh           # Prompt builder
│   ├── agent/
│   │   ├── dispatch.sh         # Render template & execute via bash
│   │   ├── loader.sh           # Load agent config from YAML
│   │   └── escape.sh           # Escape prompt for bash injection
│   ├── telemetry/
│   │   ├── events.sh           # Emit structured JSONL events
│   │   ├── effort.sh           # Cost/token tracking per bead
│   │   ├── budget.sh           # Budget enforcement and warnings
│   │   └── writer.sh           # Write to stdout and/or file
│   ├── hooks/
│   │   ├── runner.sh           # Execute hooks with environment setup
│   │   ├── validate.sh         # Validate hook scripts
│   │   └── defaults/           # Default hook templates
│   │       ├── pre-claim.sh
│   │       └── post-complete.sh
│   ├── watchdog/
│   │   ├── heartbeat.sh        # Emit heartbeat updates
│   │   ├── monitor.sh          # Background watchdog process
│   │   ├── stuck.sh            # Stuck detection logic
│   │   └── recovery.sh         # Recovery actions (kill, respawn)
│   └── lib/
│       ├── json.sh             # JSON/JSONL formatting utilities
│       ├── config.sh           # Config loading
│       └── constants.sh        # Version, URLs, defaults
├── config/
│   ├── needle.yaml.example
│   └── agents/                 # Built-in agent configurations
│       ├── claude-anthropic-sonnet.yaml
│       ├── claude-anthropic-opus.yaml
│       ├── opencode-alibaba-qwen.yaml
│       ├── opencode-zai-glm5.yaml
│       ├── opencode-ollama-deepseek.yaml
│       ├── codex-openai-gpt4.yaml
│       ├── aider-ollama-deepseek.yaml
│       └── custom-example.yaml
├── scripts/
│   └── bundle.sh               # Combine src/ into single release script
└── tests/
    ├── test_runner.sh
    ├── test_priority.sh
    ├── test_claim.sh
    ├── test_bootstrap.sh
    └── test_adapters.sh
```

### Build Process

```bash
# Development: run directly (sources files)
./bin/needle run --workspace=/path --agent=claude-anthropic-sonnet

# Release: bundle into single script
./scripts/bundle.sh > dist/needle
chmod +x dist/needle

# The bundled script embeds all src/ files and config/agents/*.yaml
```

---

## Implementation Phases

### Phase 1: Core Runner (MVP)

- [ ] `needle` CLI skeleton with subcommands
- [ ] **One-liner installer** (`curl ... | bash` from GitHub releases)
- [ ] **Interactive onboarding** (`needle init` - deps, agent detection, workspace, config)
- [ ] **Bootstrap system** (`needle setup` - auto-install tmux, jq, yq, br)
- [ ] **Global configuration** (`~/.needle/config.yaml`)
- [ ] Agent adapter system (YAML config loader)
- [ ] Adapters for: Claude Code, OpenCode, Codex, Aider
- [ ] Input method handling (stdin/file/args)
- [ ] `needle test-agent` for adapter testing
- [ ] Self-invoking tmux (`needle run` creates session)
- [ ] NATO alphabet naming (alpha, bravo, charlie...)
- [ ] Worker loop with Strand 1 (pluck) only
- [ ] Bead claim/release via `br update --claim` with retry logic
- [ ] Structured event logging (JSONL to stdout + file)
- [ ] `needle list`, `needle attach`, `needle stop`
- [ ] `needle version` (show needle + dependency versions)

### Phase 2: Full Priority System

- [ ] Strand 2 (explore): Look for work in other workspaces
- [ ] Strand 3 (mend): Maintenance (orphan cleanup, health)
- [ ] `--count=N` multi-worker launch
- [ ] **Provider/model concurrency limits** (max workers per provider-model)
- [ ] **Rate limiting** (requests per minute per provider)
- [ ] Token extraction from agent output
- [ ] Cost calculation and tracking
- [ ] Budget enforcement (warn/stop)
- [ ] Worker state registry (`~/.needle/state/workers.json`)
- [ ] **Hook system** (user-defined scripts at lifecycle events)
- [ ] **Worker heartbeat** (periodic heartbeat emission)
- [ ] **Stuck detection & auto-recovery** (watchdog process)

### Phase 3: Advanced Features

- [ ] Strand 4 (weave): Gap analysis from documentation
- [ ] Strand 5 (unravel): HUMAN alternatives
- [ ] Strand 6 (pulse): Codebase health monitoring
- [ ] Strand 7 (knot): Alert human when stuck
- [ ] Bead mitosis: Automatic decomposition for complex tasks
- [ ] Workspace auto-discovery
- [ ] `needle status` dashboard
- [ ] Configuration hot-reload
- [ ] **Billing model profiles** (pay-per-token vs use-or-lose)

---

## Success Criteria

### Phase 1 (MVP)
- [ ] **One-liner install** works: `curl -fsSL https://needle.dev/install | bash`
- [ ] **Auto-initialization**: Any command in unconfigured environment redirects to `needle init`
- [ ] **Interactive onboarding** (`needle init`) guides new users through setup
- [ ] `needle setup` bootstraps dependencies (tmux, jq, yq, br) on first run
- [ ] `needle agents --scan` detects available coding CLIs and auth status
- [ ] `needle run` launches a worker in tmux without external scripts
- [ ] `needle version` shows needle and all dependency versions
- [ ] Session names follow configurable pattern (default: `needle-{runner}-{provider}-{model}-{identifier}`)
- [ ] Global config (`~/.needle/config.yaml`) controls all behavior
- [ ] **Adding a new agent requires only a YAML config file (no code changes)**
- [ ] Built-in adapters work for Claude Code, OpenCode, Codex, and Aider
- [ ] `needle test-agent <name>` validates adapter configuration
- [ ] `needle list` can filter by runner, provider, or model

### Phase 2 (Full System)
- [ ] Multiple workers run in parallel, competing for beads atomically
- [ ] Provider/model concurrency limits enforced (`limits.providers`, `limits.models`)
- [ ] Rate limiting prevents API throttling (`requests_per_minute`)
- [ ] Priority engine, bead manager, and effort logger are independently testable
- [ ] Effort logs capture runner/provider/model metadata for cost analysis
- [ ] Workers auto-scale when backlog exceeds threshold
- [ ] Workers clean up stale claims from crashed workers

### Phase 3 (Advanced)
- [ ] Billing model (`pay_per_token`, `use_or_lose`, `unlimited`) adjusts priority behavior
- [ ] Workspace config (`.needle.yaml`) overrides global settings
- [ ] Strands 4-6 (weave, unravel, knot) opt-in features work correctly
- [ ] `needle status` dashboard shows real-time worker state
