# Agent Onboarding: NEEDLE Quickstart

This is a complete, agent-readable runbook for setting up NEEDLE from scratch. Every command includes expected output and common failure modes with their fixes.

## What NEEDLE is

NEEDLE (Navigates Every Enqueued Deliverable, Logs Effort) is a deterministic state machine that drives headless coding CLI agents. It processes a shared bead queue in priority order, dispatches work to any agent CLI (Claude Code, OpenCode, Codex, Aider), and handles every outcome through an explicit path.

## Prerequisites

- Linux x86_64 (prebuilt binaries; everything else builds from source)
- `git`, `tmux` on PATH
- An agent CLI on PATH — this guide uses [Claude Code](https://claude.ai/code) (`claude`)

## Step 1: Install needle, bead-rs backend, and transform helpers

```bash
curl -fsSL https://github.com/jedarden/NEEDLE/releases/latest/download/install.sh | bash
```

**Expected behavior:**
- Downloads and installs `needle`, `bead` (bead-rs backend), and transform helper binaries
- Places binaries in `~/.local/bin` (override with `NEEDLE_INSTALL_PATH`)
- Verifies SHA-256 checksums (enabled by default; `--skip-checksum` for emergencies only)

**Expected output:**
```
Installing NEEDLE to /home/user/.local/bin...
Downloading needle-x86_64-unknown-linux-gnu...
Downloading bead-x86_64-unknown-linux-gnu...
Downloading transform helpers...
Verifying checksums... OK
Installation complete.
Run 'needle doctor' to verify your setup.
```

**Failure modes (2026-08-29):**

| Symptom | Cause | Fix |
|---------|-------|-----|
| `command not found: needle` | `~/.local/bin` not on PATH | Add `export PATH="$HOME/.local/bin:$PATH"` to `~/.bashrc` or `~/.zshrc` and source it |
| `checksum verification failed` | Corrupted download or tampered binary | Rerun install.sh; if persistent, use `--skip-checksum` (emergency only) |
| `permission denied` | No write access to `~/.local/bin` | Create directory first: `mkdir -p ~/.local/bin` |
| `curl: command not found` | `curl` not installed | Install curl: `sudo apt install curl` (Debian/Ubuntu) or `sudo yum install curl` (RHEL/Fedora) |

**Verification:**
```bash
which needle  # Should print: /home/user/.local/bin/needle
which bead    # Should print: /home/user/.local/bin/bead
needle --version
bead --version
```

## Step 2: Initialize your repo with the bead-rs backend

```bash
cd <your-repo> && needle init --backend bead-rs
```

**Expected behavior:**
- Creates `.needle.yaml` in the workspace root (binds repo to bead backend)
- Creates `~/.config/needle/config.yaml` if absent (global worker configuration)
- Creates `AGENTS.md` with a "Working with beads" section (if not present)

**Expected output:**
```
Initialized workspace at /home/user/myrepo with bead-rs backend
Created .needle.yaml
Created ~/.config/needle/config.yaml (global config)
AGENTS.md already exists — skipping bead-workflow section
```

**Failure modes:**

| Symptom | Cause | Fix |
|---------|-------|-----|
| `error: not a git repository` | Current directory is not a git repo | Run `git init` first or change to a git directory |
| `error: .needle.yaml already exists` | Workspace already initialized | Existing config is valid; no action needed |
| `permission denied: ~/.config/needle` | No write access to config directory | Create directory with permissions: `mkdir -p ~/.config/needle` |

**Verification:**
```bash
cat .needle.yaml  # Should show bead_cli.backend: bead-rs
cat ~/.config/needle/config.yaml  # Should exist with default config
```

## Step 3: Create the bead store

```bash
bead init --prefix <name>
```

**Expected behavior:**
- Creates `.beads/` directory structure
- Initializes `beads.db` (SQLite database)
- Creates checkpoint structure: `.beads/checkpoint/{current.json,forensic.jsonl,objects/}`

**Expected output:**
```
Initialized bead store with prefix 'myproject'
Created .beads/beads.db
Created checkpoint structure at .beads/checkpoint/
Store ready. Run 'bead create' to add your first bead.
```

**Failure modes (2026-08-29):**

| Symptom | Cause | Fix |
|---------|-------|-----|
| `command not found: bead` | Install.sh failed or PATH not updated | Verify install: `ls ~/.local/bin/bead`; if missing, rerun install.sh |
| `error: .beads already exists` | Bead store already initialized | Existing store is valid; no action needed |
| `permission denied: .beads` | No write access to current directory | Check directory permissions: `ls -lad .` |
| `sqlite3: error while loading shared libraries` | Missing SQLite library | Install: `sudo apt install libsqlite3-dev` (Debian) or `sudo yum install sqlite-devel` (RHEL) |

**Verification:**
```bash
ls -la .beads/
# Output should show:
# .beads/beads.db
# .beads/checkpoint/current.json
# .beads/checkpoint/forensic.jsonl
# .beads/checkpoint/objects/
```

## Step 4: Create your first bead

```bash
bead create --title "Add a CONTRIBUTING.md" --priority 2
```

**Expected behavior:**
- Creates a new bead in the store
- Prints the bead ID (format: 8-character alphanumeric, e.g., `abc123de`)
- Bead starts with status `open` and no assignee

**Expected output:**
```
Created bead abc123de
Title: Add a CONTRIBUTING.md
Priority: 2
Status: open
```

**Failure modes:**

| Symptom | Cause | Fix |
|---------|-------|-----|
| `error: bead store not initialized` | Step 3 (`bead init`) was skipped | Run `bead init --prefix <name>` first |
| `error: invalid priority` | Priority outside 0-4 range | Use `--priority 0` (lowest) through `--priority 4` (highest) |
| `error: database locked` | Another process has beads.db open | Wait and retry; ensure no other workers running |

**Verification:**
```bash
bead list --status open
# Should show the new bead in the list
```

## Step 5: Verify system health

```bash
needle doctor
```

**Expected behavior:**
- Runs comprehensive health checks
- All rows show `PASS` or `WARN` (no `FAIL` rows)
- Exit code is `0`

**Expected output (real output from needle 0.6.0 + bead 0.2.4):**

```
NEEDLE Doctor
────────────────────────────────────────────────────────────
[PASS]  Config                        valid
[PASS]  Workspace                     /home/user/myrepo
[WARN]  SQLite integrity              sqlite3 not on PATH — skipped
[PASS]  Lock files                    none
[PASS]  Bead CLI Backend              bead-rs
         └─ CLI path: ~/.local/bin/bead
         └─ source: config file
         └─ verified against: bead 0.2.4 (commit abc123)
         └─ capability gap: split/mitosis is sequential, not atomic
         └─ capability gap: claim omits model/harness velocity metadata
[PASS]  Bead store                    ok
[PASS]  Checkpoint                    native pointer is valid JSON
[PASS]  Worker registry               empty
[WARN]  Heartbeat dir                 missing: ~/.needle/state/heartbeats
[PASS]  Heartbeat files               no heartbeat directory
[PASS]  Peers                         no workers running
[PASS]  Agent binary                  claude at ~/.local/bin/claude
[PASS]  Adapter transforms            ok
[PASS]  Adapter template executables  all commands available
[PASS]  Disk space                    <n> MB available
[PASS]  Telemetry logs                no log directory yet
────────────────────────────────────────────────────────────
14 passed, 2 warning(s), 0 failure(s).
Run `needle doctor --repair` to attempt automatic fixes.
```

**Failure modes (2026-08-29):**

| Symptom | Cause | Fix |
|---------|-------|-----|
| `[FAIL] Config` | `~/.config/needle/config.yaml` missing or invalid | Run `needle init --backend bead-rs` to recreate |
| `[FAIL] Workspace` | Not in a git repository or `.needle.yaml` missing | Run `git init` and `needle init --backend bead-rs` |
| `[FAIL] Bead CLI Backend` | `bead` command not on PATH or wrong version | Rerun install.sh; verify with `which bead` |
| `[FAIL] Agent binary` | `claude` CLI not installed or not on PATH | Install Claude Code: `npm install -g @anthropics/claude-code` |
| `[FAIL] Adapter template executables` | Missing shell commands (bash, mktemp) | Install missing dependencies: `sudo apt install bash mktemp` |

**Key point:** Every `FAIL` row names its specific fix. Follow the fix command printed in the row.

## Step 6: Run a worker

```bash
needle run --agent claude --identifier alpha
```

**Expected behavior:**
- Starts a worker in a detached tmux session (`needle-claude-alpha`)
- Worker claims the next available bead
- Dispatches bead to Claude Code
- Monitors execution and handles outcome
- Closes bead on success, releases on failure
- Loops until no beads remain

**Expected output (first cycle):**

```
[2026-08-29 12:34:56] NEEDLE worker starting...
[2026-08-29 12:34:56] Worker identity: needle-claude-alpha
[2026-08-29 12:34:56] Workspace: /home/user/myrepo
[2026-08-29 12:34:56] Agent: claude
[2026-08-29 12:34:56] Attaching to tmux session: needle-claude-alpha

[2026-08-29 12:34:57] 🔍 SELECT: querying bead store...
[2026-08-29 12:34:57]    Found 1 open beads
[2026-08-29 12:34:57]    Ready frontier: 1 beads
[2026-08-29 12:34:57]    Selected: abc123de (priority 2, created 2026-08-29T12:30:00Z)

[2026-08-29 12:34:57] 🔒 CLAIM: attempting atomic claim...
[2026-08-29 12:34:57]    Claim successful: abc123de → in_progress
[2026-08-29 12:34:57]    Assignee: needle-claude-alpha

[2026-08-29 12:34:57] 📋 BUILD: constructing prompt...
[2026-08-29 12:34:58]    Bead: Add a CONTRIBUTING.md
[2026-08-29 12:34:58]    Context: README.md (existing)

[2026-08-29 12:34:58] 🚀 DISPATCH: invoking agent...
[2026-08-29 12:34:58]    Agent: claude
[2026-08-29 12:34:58]    Command: claude -p … --dangerously-skip-permissions
[2026-08-29 12:34:58]    Timeout: 300s

[2026-08-29 12:34:58] ⏳ EXECUTE: agent running...
[2026-08-29 12:35:45]    Agent exited: code 0 (success)
[2026-08-29 12:35:45]    Duration: 47.2s

[2026-08-29 12:35:45] 📊 OUTCOME: processing success...
[2026-08-29 12:35:45]    Validating output...
[2026-08-29 12:35:45]    ✓ Changes detected: CONTRIBUTING.md (new file)
[2026-08-29 12:35:45]    ✓ Git commit created
[2026-08-29 12:35:45]    Closing bead: abc123de
[2026-08-29 12:35:45]    ✓ Closed successfully

[2026-08-29 12:35:45] ─── Cycle complete in 48.1s ───
```

**Failure modes (2026-08-29):**

| Symptom | Cause | Fix |
|---------|-------|-----|
| `error: unknown adapter: claude` | `claude` CLI not installed or not on PATH | Install Claude Code: `npm install -g @anthropics/claude-code` |
| `error: bead store not initialized` | Step 3 (`bead init`) was skipped | Run `bead init --prefix <name>` first |
| `worker exits immediately` | No open beads or config error | Check: `bead list --status open`; verify `~/.config/needle/config.yaml` |
| `bead stuck in in_progress` | Agent crashed or hung during execution | Manually release: `bead release <id>`; check agent logs |
| `tmux: session already exists` | Previous worker still running | Attach: `tmux attach -t needle-claude-alpha` or kill: `tmux kill-session -t needle-claude-alpha` |

**Important heads-up:** The built-in `claude` adapter invokes `claude -p … --dangerously-skip-permissions`. Unattended operation means no permission prompts; read `needle config` before pointing a worker at a repository you care about.

## Step 7: Check status and attach to the session

```bash
needle status
tmux attach -t needle-claude-alpha
```

**Expected `needle status` output:**
```
Active workers:
  needle-claude-alpha (running since 2026-08-29T12:34:56Z)
    Current bead: abc123de (in_progress)
    Agent: claude
    Session: /tmp/claude-session-abc123
```

**Expected `tmux attach` output:**
- Attaches to the live worker session
- Shows agent running in real-time
- Press `Ctrl+B D` to detach without stopping the worker

**Failure modes:**

| Symptom | Cause | Fix |
|---------|-------|-----|
| `no active workers` | Worker exited or not started | Check if beads remain: `bead list --status open` |
| `session not found: needle-claude-alpha` | Worker crashed or never started | Check logs: `ls -la ~/.needle/state/heartbeats/` |
| `tmux: command not found` | tmux not installed | Install: `sudo apt install tmux` (Debian) or `sudo yum install tmux` (RHEL) |

## Step 8: Verify completion

```bash
bead list --status closed
```

**Expected output:**
```
ID           TITLE                    STATUS    CLOSED_AT                    ASSIGNee
abc123de    Add a CONTRIBUTING.md     closed    2026-08-29T12:35:45Z         needle-claude-alpha
```

**Verification checks:**
```bash
# Check the file was created
ls -la CONTRIBUTING.md

# Check git history
git log --oneline -1
# Should show commit with bead ID trailer:
# abc123de fix(abc123de): add CONTRIBUTING.md

# Verify the bead closed successfully
bead show abc123de
```

## If you are the agent working beads in this repo

If you're an AI agent processing beads in a NEEDLE workspace, read the workspace-specific instructions in `AGENTS.md`. That file contains:

- Workspace-specific conventions and constraints
- Commit message formats
- Testing requirements
- Bead workflow specifics

The "Working with beads" section in `AGENTS.md` is your primary reference for how to work in this particular repository.

## Build from source (alternative to prebuilt binaries)

If prebuilt binaries don't work for your platform or you prefer to build:

```bash
# Rust 1.85+ required (NEEDLE pins its toolchain in rust-toolchain.toml)
cargo install --git https://github.com/jedarden/NEEDLE
cargo install --git https://github.com/jedarden/bead-rs --bin bead
```

This installs from source and may take several minutes.

## Next steps

- Configure multiple agents: edit `~/.config/needle/config.yaml`
- Run multiple workers: `needle run --agent claude --count 3`
- Add bead dependencies: `bead dep add <dependent> <blocks>`
- Explore strand escalation (auto-discovery, health checks, gap analysis): see `docs/plan/plan.md`
- Wire up OpenTelemetry telemetry: see `docs/configuration.md`

## Complete documentation index

- [Configuration reference](docs/configuration.md) — adapter YAML schema, all config options
- [Documentation index](docs/README.md) — ADRs, architecture, operations, investigations
- [Integration tests](tests/) — Comprehensive test suite demonstrating all functionality
- [Quickstart example](docs/examples/quickstart/) — Minimal workspace configuration with seed beads

## Quick reference card

```bash
# Lifecycle commands
needle init --backend bead-rs          # Initialize workspace
bead init --prefix <name>             # Create bead store
bead create --title "..." --priority 2  # Create bead
needle doctor                         # Verify health
needle run --agent claude -i alpha    # Start worker
needle status                         # List workers
bead list [--status open|closed]      # List beads
bead show <id>                        # Bead details
bead release <id>                     # Release stuck bead
```

---

**This document is maintained in sync with llms.txt and README.md Quickstart.** If commands diverge, that's a bug — open an issue or submit a PR.
