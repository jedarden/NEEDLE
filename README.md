# NEEDLE

**N**avigates **E**very **E**nqueued **D**eliverable, **L**ogs **E**ffort

NEEDLE is a universal wrapper for headless coding CLI agents. It provides a priority-based task queue, parallel worker orchestration, and effort logging — all without external coordination services.

## Overview

- **Agent Abstraction** — Unified interface for Claude Code, OpenCode, Codex CLI, Aider, and any headless coding CLI
- **Task Navigation** — Priority-weighted bead queue processing across one or more workspaces
- **Effort Logging** — Time, tokens, and cost tracking per deliverable
- **Parallel Workers** — Multiple workers run independently via tmux; SQLite provides atomic claim semantics
- **Live Dashboard** — FABRIC real-time dashboard (`needle dashboard start`) shows active workers, bead throughput, cost, and a live event stream; no external services required

NEEDLE wraps any headless CLI that can execute a prompt and exit. Workers claim beads, dispatch to the configured agent, and record results. NEEDLE auto-creates tmux sessions for session persistence.

## Installation

### Prerequisites

| Dependency | Purpose | Auto-installed |
|------------|---------|----------------|
| `bash` 4.4+ | Shell execution | No (system) |
| `tmux` 3.0+ | Session management | **Yes** |
| `jq` 1.6+ | JSON parsing | **Yes** |
| `yq` 4.0+ | YAML parsing | **Yes** |
| `br` (beads_rust) | Bead queue management | **Yes** |

At least one supported coding agent must be installed:

| Agent | Install |
|-------|---------|
| Claude Code | `npm install -g @anthropic-ai/claude-code` |
| OpenCode | `go install github.com/opencode-ai/opencode@latest` |
| Codex CLI | `npm install -g @openai/codex` |
| Aider | `pip install aider-chat` |

### Install NEEDLE

```bash
# One-liner install (downloads binary, adds to PATH, runs needle init)
curl -fsSL https://needle.dev/install | bash
```

Or manually:

```bash
curl -fsSL https://github.com/user/needle/releases/latest/download/needle \
  -o ~/.local/bin/needle
chmod +x ~/.local/bin/needle
needle init
```

`needle init` installs missing dependencies, detects available agents, and creates `~/.needle/config.yaml`.

## Quick Start

```bash
# Interactive setup (first run)
needle init

# Start a worker in the current workspace
needle run

# Start a worker with explicit options
needle run --workspace=/path/to/project --agent=claude-anthropic-sonnet

# Start multiple parallel workers
needle run --workers=3 --workspace=/path/to/project

# Check running workers
needle list

# Attach to a worker session
needle attach alpha

# Stop all workers
needle stop --all
```

## Configuration

Configuration layers (lowest → highest precedence):

1. Built-in defaults
2. Global config — `~/.needle/config.yaml`
3. Workspace config — `<workspace>/.needle.yaml`
4. Environment variables — `NEEDLE_*` prefix

### `~/.needle/config.yaml`

```yaml
agent: claude-anthropic-sonnet
workspace: /path/to/project
workers: 1

strands:
  pluck: true        # Primary work from configured workspaces
  explore: auto      # Discover work in other workspaces
  mend: auto         # Heartbeat checks, log rotation
  weave: auto        # Create beads from documentation gaps
  unravel: auto      # Propose alternatives for blocked beads
  pulse: auto        # Proactive quality monitoring
  knot: auto         # Alert humans when worker is stuck

billing_model: pay_per_token  # or: subscription, free
```

### Environment Variable Overrides

```bash
NEEDLE_AGENT=opencode-alibaba-qwen      # Override active agent
NEEDLE_WORKSPACE=/path/to/project       # Override workspace
NEEDLE_WORKERS=4                        # Override worker count
NEEDLE_LOG_LEVEL=debug                  # Log verbosity
```

### Workspace Bead Config (`.beads/config.yaml`)

```yaml
issue_prefix: nd
default_priority: 2
default_type: task
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `needle init` | Interactive setup: install deps, detect agents, create config |
| `needle run` | Start worker(s) in tmux sessions |
| `needle list` | List running worker sessions |
| `needle attach <id>` | Attach to a worker tmux session |
| `needle stop [--all]` | Stop one or all workers |
| `needle agents` | List configured agent adapters |
| `needle agents --scan` | Re-scan PATH for available agents |
| `needle setup` | Re-check and install dependencies |
| `needle dashboard start` | Start the FABRIC real-time dashboard server |
| `needle dashboard status` | Check if the dashboard is running |
| `needle dashboard stop` | Stop the dashboard server |
| `needle dashboard logs` | View dashboard logs |
| `needle upgrade` | Download and install latest NEEDLE version |
| `needle upgrade --check` | Check if a newer version is available |
| `needle version` | Print current version |
| `needle help` | Show full command reference |

### `needle run` Options

```
--workspace=PATH     Workspace directory containing .beads/
--agent=NAME         Agent adapter to use (e.g. claude-anthropic-sonnet)
--workers=N          Number of parallel workers to start (default: 1)
--identifier=ID      Worker name suffix (default: auto-assigned alpha/bravo/...)
--non-interactive    Skip prompts; use defaults or provided flags
```

## Architecture

NEEDLE has seven work-finding strategies called **strands**, tried in priority order each iteration:

| Priority | Strand | Purpose |
|----------|--------|---------|
| 1 | Pluck | Claim and execute beads from configured workspaces |
| 2 | Explore | Discover work in other workspaces |
| 3 | Mend | Maintenance: heartbeat checks, log rotation |
| 4 | Weave | Create beads from documentation gaps |
| 5 | Unravel | Propose alternatives for blocked beads |
| 6 | Pulse | Proactive quality monitoring (security, deps, coverage) |
| 7 | Knot | Alert humans when the worker is persistently stuck |

The engine stops at the first strand that finds work. If all strands report no work, the worker sleeps and retries.

**Bead claim atomicity** is provided by `br update --claim` (SQLite transactions). When multiple workers race to claim the same bead, exactly one succeeds; others retry with the next available bead.

For a detailed design reference, see [ARCHITECTURE.md](ARCHITECTURE.md) and [docs/plan.md](docs/plan.md).

## State and Logs

NEEDLE stores runtime state in `~/.needle/`:

```
~/.needle/
├── config.yaml          # User configuration
├── agents/              # Custom agent adapters
├── logs/                # JSONL event logs per session
├── state/
│   ├── workers.json     # Active worker registry
│   ├── heartbeats/      # Worker liveness files
│   └── pulse/           # Codebase health scan state
├── hooks/               # Lifecycle hooks (pre-claim, post-execute, ...)
├── cache/               # Downloaded binaries and update artifacts
├── dashboard.pid        # PID file for the dashboard server (when running)
└── logs/dashboard.log   # Dashboard server log output
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, project structure, strand authoring, agent adapter specs, and the PR process.

## Links

- [beads_rust](https://github.com/Dicklesworthstone/beads_rust) — Bead queue CLI
- [ARCHITECTURE.md](ARCHITECTURE.md) — Internal system design
- [docs/plan.md](docs/plan.md) — Full implementation plan
- [ROADMAP.md](ROADMAP.md) — Planned features
