# NEEDLE Quickstart Example

This is a complete, end-to-end walkthrough from empty workspace to first closed bead. Follow every step exactly — this example uses a throwaway project so you can safely run it anywhere.

## Prerequisites

You need:
- `needle` installed (see [main README](https://github.com/jedarden/NEEDLE))
- `bead` CLI installed (bead-rs backend)
- A Claude Code CLI on your `$PATH` (any agent works, but we'll use Claude)

## Step 0: Sandbox your HOME (mandatory)

This walkthrough writes `~/.config/needle/config.yaml`. Never run it against a
HOME that already runs NEEDLE — on 2026-08-30 a worker did exactly that and
replaced a fleet's global config with the one-worker example below. Use a
throwaway HOME for the whole example:

```bash
export HOME="$(mktemp -d /tmp/needle-quickstart-home.XXXXXX)"
export PATH="$HOME/.local/bin:$PATH"
```

## Step 1: Create a Throwaway Workspace

We'll use a disposable project so the agent has harmless work to do:

```bash
# Create and enter a temporary workspace
mkdir -p /tmp/needle-quickstart-project
cd /tmp/needle-quickstart-project

# Initialize a minimal git repo (needed for bead operations)
git init
git config user.email "quickstart@example.com"
git config user.name "Quickstart User"

# Create a minimal README so we have something to work on
cat > README.md << 'EOF'
# Quickstart Test Project

This is a disposable project for the NEEDLE quickstart example.
EOF

git add README.md
git commit -m "Initial commit"
```

## Step 2: Configure the Workspace

Bind the workspace to its bead backend. This writes `./.needle.yaml`, the global
`~/.config/needle/config.yaml` (if absent), and a "Working with beads" section in
`./AGENTS.md` for any coding agent that works in this repo:

```bash
needle init --backend bead-rs
```

Then narrow the sandbox HOME's global config to a single worker that exits when
the queue is empty (this overwrites the file `needle init` just created — which
is fine only because HOME is the sandbox from Step 0):

```bash
cat > ~/.config/needle/config.yaml << 'EOF'
# Minimal worker config for quickstart
agent:
  default: claude
  timeout: 300  # 5 minute timeout per bead

worker:
  max_workers: 1           # Only one worker for this example
  idle_timeout: 30         # Check for work every 30 seconds
  idle_action: exit        # Exit when no work remains
  identifier_scheme: sequential   # Use sequential identifiers (worker-1, worker-2, ...)
EOF
```

## Step 3: Initialize the Bead Store

```bash
# Initialize the bead store
bead init --prefix quickstart

# Verify everything resolves
needle doctor
```

**Expected `needle doctor` output** (real output from needle 0.6.0 + bead 0.2.2 on a clean host; paths shortened):

```
NEEDLE Doctor
────────────────────────────────────────────────────────────
[PASS]  Config                        valid
[PASS]  Workspace                     /tmp/needle-quickstart-project
[WARN]  SQLite integrity              sqlite3 not on PATH — skipped
[PASS]  Lock files                    none
[PASS]  Bead CLI Backend              bead-rs
         └─ CLI path: ~/.local/bin/bead
         └─ source: config file
         └─ verified against: bead 0.1.3 (commit 85f36ac)
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

Every row is `PASS` or `WARN` and the exit code is 0. A `FAIL` row names the fix.

## Step 4: Seed Test Beads

Run the provided seed script to create three test beads with one dependency:

```bash
# From the NEEDLE repo (adjust path if needed)
bash /path/to/NEEDLE/docs/examples/quickstart/seed-beads.sh
```

Or create them manually:

```bash
# Create three sequential beads
contributing_id=$(bead create --title "Add CONTRIBUTING.md" --priority 2 --issue-type task)
license_id=$(bead create --title "Add LICENSE file" --priority 2 --issue-type task)
makefile_id=$(bead create --title "Add simple Makefile" --priority 1 --issue-type task)

# Add a dependency: Makefile depends on LICENSE
bead dep add "$makefile_id" "$license_id"
```

## Step 5: Run the Worker

Start a single worker and watch it process the beads:

```bash
# Run one worker
needle run --agent claude -i alpha
```

**What you'll see:**

The worker will:
1. Start in its own tmux session (`tmux ls` shows it; `needle status` lists it)
2. Select the next claimable bead
3. Dispatch it to Claude Code
4. Wait for the agent to complete the work
5. Close the bead on success
6. Move to the next bead
7. Exit when no work remains

## Step 6: Verify Results

After the worker exits, check what was accomplished:

```bash
# List closed beads
bead list --status closed

# Check what files were created
ls -la

# See the git history
git log --oneline
```

**Expected final state:**
- Three beads with status `closed`
- Three new files: `CONTRIBUTING.md`, `LICENSE`, `Makefile`
- Three git commits, one per bead

## What Just Happened?

1. **Selection**: NEEDLE queried the bead store for the next claimable bead (priority order, oldest first)
2. **Claim**: Atomically claimed the bead via `bead claim` (SQLite transaction guarantees only one worker wins)
3. **Build**: Constructed a prompt from the bead's context (title, body, workspace files)
4. **Dispatch**: Invoked Claude Code headless with the prompt
5. **Execute**: Claude ran, made changes, and exited with code 0 (success)
6. **Outcome**: NEEDLE validated the output, committed changes, and closed the bead

## Troubleshooting

**`needle doctor` fails:**
- Ensure `bead` is on your `$PATH` (`which bead`)
- Check that `.needle.yaml` exists in your workspace
- Verify `claude` CLI is installed (`which claude`)

**Worker exits immediately:**
- Check if beads exist: `bead list --status open`
- Verify the workspace has a git repo: `git status`

**Beads stuck in `in_progress`:**
- Something went wrong during dispatch. Check the bead:
  ```bash
  bead show <id>
  ```
- Manually release stuck beads:
  ```bash
  bead release <id>
  ```

## Cleanup

When you're done experimenting:

```bash
# Exit the workspace
cd /

# Remove the temporary project
rm -rf /tmp/needle-quickstart-project
```

## Next Steps

- Try multiple workers: `needle run --agent claude --count 3`
- Note the built-in `claude` adapter runs with `--dangerously-skip-permissions` — expected for unattended work, but read `needle config` first
- Add more beads with dependencies: `bead dep add <dependent> <blocks>`
- Configure different agents in `~/.config/needle/config.yaml`
- See [main README](https://github.com/jedarden/NEEDLE) for full documentation

---

**This is a teaching example.** The beads are simple, the config is minimal, and the project is disposable. Real-world use involves more complex workspaces, but the core loop — select, claim, dispatch, execute, outcome — is identical.
