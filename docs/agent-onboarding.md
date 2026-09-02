# Agent Onboarding Guide

This guide walks you through setting up NEEDLE from a clean environment to your first closed bead, with expected output at each step and common failure modes.

## Prerequisites

- Linux x86_64 system (prebuilt binaries) or Rust 1.85+ for source build
- `git` and `tmux` installed
- An agent CLI on PATH (this guide uses Claude Code: `claude`)

## Step-by-Step Setup

### Step 1: Install NEEDLE and dependencies

```bash
curl -fsSL https://github.com/jedarden/NEEDLE/releases/latest/download/install.sh | bash
```

**Expected output:**
```
Installing needle to /home/user/.local/bin...
Installing bead-rs to /home/user/.local/bin...
✓ needle installed successfully
✓ bead-rs installed successfully
Add ~/.local/bin to your PATH if needed
```

**Common failures:**
- `curl: command not found` → Install curl: `sudo apt install curl`
- `Permission denied` → Check write permissions on `~/.local/bin`
- `Checksum verification failed` → Network issue; retry or use `--skip-checksum` (not recommended for production)

---

### Step 2: Initialize your workspace

```bash
cd <your-repo> && needle init --backend bead-rs
```

**Expected output:**
```
✓ Created .needle.yaml with bead-rs backend
✓ Workspace initialized at /path/to/your-repo
Run 'bead init --prefix <name>' to create the bead store
```

**Common failures:**
- `needle: command not found` → Add `~/.local/bin` to PATH: `export PATH="$HOME/.local/bin:$PATH"`
- `No such file or directory` → Ensure you're in a valid git repository

---

### Step 3: Create the bead store

```bash
bead init --prefix <name>
```

**Expected output:**
```
✓ Created .beads/ directory structure
✓ Initialized SQLite database at .beads/beads.db
✓ Checkpoint enabled
Bead store ready
```

**Common failures:**
- `bead: command not found` → The installer failed; run: `curl -fsSL https://github.com/jedarden/NEEDLE/releases/latest/download/install.sh | bash`
- `Permission denied` → Check directory write permissions
- `SQLite error` → Ensure `sqlite3` is available: `sudo apt install sqlite3`

---

### Step 4: Create your first bead

```bash
bead create --title "Add a CONTRIBUTING.md" --priority 2
```

**Expected output:**
```
✓ Created bead needle-xxxxxxxx (open)
Title: Add a CONTRIBUTING.md
Priority: 2
Status: open
```

**Common failures:**
- `bead: command not found` → Same as Step 3
- `Invalid priority` → Use 0-4 (0=highest, 4=lowest)

---

### Step 5: Verify system health

```bash
needle doctor
```

**Expected output:**
```
✓ binary fresh        PASS   needle is latest version
✓ config valid        PASS   .needle.yaml is valid
✓ backend reachable  PASS   bead store initialized
✓ capabilities OK    PASS   bead-rs supports required features
✓ workspace clean    PASS   no uncommitted changes
All checks passed
```

**Common failures (2026-08-29 incident):**
- `bead: command not found` in `binary fresh` FAIL row → Reinstall: `curl -fsSL https://github.com/jedarden/NEEDLE/releases/latest/download/install.sh | bash`
- `unknown adapter` in `config valid` FAIL row → Check adapter name in `.needle.yaml` matches installed adapters
- `capabilities OK` FAIL row → Update bead-rs: `cargo install --git https://github.com/jedarden/bead-rs --bin bead`

---

### Step 6: Run a worker

```bash
needle run --agent claude --identifier alpha
```

**Expected output:**
```
Starting worker needle-claude-alpha...
✓ Worker launched in tmux session needle-claude-alpha
Claiming next bead...
```

**Common failures:**
- `claude: command not found` → Install Claude Code: `npm install -g @anthropic-ai/claude-code`
- `unknown adapter claude` → Check adapter configuration in `.needle.yaml`
- `tmux: command not found` → Install tmux: `sudo apt install tmux`

---

### Step 7: Monitor the worker

```bash
needle status
tmux attach -t needle-claude-alpha
```

**Expected output from `needle status`:**
```
Active workers:
  needle-claude-alpha  (running)  working on needle-xxxxxxxx
```

**Expected output from tmux attach:**
```
[needle-claude-alpha] Claimed bead needle-xxxxxxxx
[needle-claude-alpha] Dispatching to claude...
[agent output appears here]
```

**Common failures:**
- `no active workers` → Worker crashed; check logs in `.beads/events.jsonl`
- `session not found` → Worker hasn't started or already exited

---

### Step 8: Verify completion

```bash
bead list --status closed
```

**Expected output:**
```
Closed beads:
needle-xxxxxxxx  Add a CONTRIBUTING.md  closed  2026-08-29T12:34:56Z
```

**Common failures:**
- `no closed beads` → Worker failed; check bead status: `bead show needle-xxxxxxxx`

---

## If You Are the Agent Working Beads in This Repo

You're reading this because an agent told you to set up NEEDLE. If you're now the agent processing beads in this repository, read the AGENTS.md template for workspace-specific conventions:

```bash
cat AGENTS.md
```

Key points for agents working NEEDLE beads:
- Always commit changes with bead ID in commit message: `feat(needle-XYZ): description`
- Never modify `.beads/` manually — use `bead` CLI
- Isolate tests (see `docs/testing-isolation-patterns.md`)
- Run `cargo test` remotely via iad-ci, never locally
- Close beads with `bead close <id> --reason "..."`

---

## Troubleshooting

### Worker won't start
```bash
# Check if tmux session exists
tmux ls

# Kill stuck sessions
tmux kill-server

# Restart worker
needle run --agent claude --identifier alpha
```

### Bead stuck in `in_progress`
```bash
# Check bead status
bead show <id>

# Release stale assignment
bead release <id>
```

### Verify binary freshness
```bash
needle doctor  # Should show PASS for binary fresh
```

### Full diagnostic mode
```bash
needle doctor --verbose
```

---

## Next Steps

- **Configuration:** See `docs/configuration.md` for adapter YAML schema and all config options
- **Documentation:** See `docs/README.md` for complete documentation index (ADRs, architecture, operations)
- **Examples:** See `docs/examples/quickstart/` for minimal workspace configuration
- **Testing:** See `tests/` for integration test examples
