# NEEDLE Quickstart Example

This is a complete, end-to-end walkthrough from empty workspace to first closed bead. Follow every step exactly — this example uses a throwaway project so you can safely run it anywhere.

## Prerequisites

You need:
- `needle` installed (see [main README](https://github.com/jedarden/NEEDLE))
- `bead` CLI installed (bead-rs backend)
- A Claude Code CLI on your `$PATH` (any agent works, but we'll use Claude)

## Step 0: Sandbox your HOME (mandatory)

This walkthrough writes `~/.config/needle/config.yaml` — the file NEEDLE's
loader reads on every host. Never run it against a HOME that already runs
NEEDLE: on 2026-08-30 a worker did exactly that and replaced a fleet's global
config with the one-worker example below (one worker, `idle_action: exit`, and
the Anthropic-billing agent default). Use a throwaway HOME for the entire
example:

```bash
# The binaries live in the real HOME. Remember where they are before HOME moves.
QUICKSTART_BIN="$(dirname "$(command -v needle)")"

# Move HOME to a throwaway directory. Everything below runs against it.
export HOME="$(mktemp -d /tmp/needle-quickstart-home.XXXXXX)"
export PATH="$QUICKSTART_BIN:$PATH"

# All three must still resolve; if one does not, add its directory to PATH too.
command -v needle bead claude
```

The sandbox lasts only for this shell — a new terminal returns you to your real
HOME. Do not carry it into any other work, and never point an existing NEEDLE
host at the example.

## Step 1: Create a Throwaway Workspace

We'll use a disposable project so the agent has harmless work to do. Besides a git
repo it needs **a remote to push to** — see "The shipped-work gate" below for why.
A local bare repository plays that role, so no hosting account is required:

```bash
# Create and enter a temporary workspace
mkdir -p /tmp/needle-quickstart-project
cd /tmp/needle-quickstart-project

# A local bare repository stands in for a hosted remote (GitHub, Forgejo, …)
git init --bare -b main /tmp/needle-quickstart-remote.git

# Initialize a minimal git repo (needed for bead operations)
git init -b main
git config user.email "quickstart@example.com"
git config user.name "Quickstart User"

# Create a minimal README so we have something to work on
cat > README.md << 'EOF'
# Quickstart Test Project

This is a disposable project for the NEEDLE quickstart example.
EOF

git add README.md
git commit -m "Initial commit"

# Publish the branch and set its upstream in one step
git remote add origin /tmp/needle-quickstart-remote.git
git push -u origin main
```

### The shipped-work gate

NEEDLE does not accept a bead closure on faith. With the default
`worker.enforce_shipped_work: true`, a closure counts only if the dispatch
produced a commit — on a path outside `notes/` and `.beads/` — that has been
**pushed to the branch's upstream**. (The prompt NEEDLE builds instructs the
agent to commit and push; an agent that legitimately ships no code instead
records an explanatory note on the bead.) A branch with no upstream — a plain
`git init`, or a remote added without `git push -u` — gives the gate nothing to
compare against, so it cannot verify any closure there. That is why Step 1 ends
with `git push -u origin main`.

If your repository is genuinely local-only and will never push, say so
explicitly instead of leaving the gate unable to run:

```yaml
# The sandbox HOME's ~/.config/needle/config.yaml (Step 0's sandbox — never a
# real host's fleet config)
worker:
  enforce_shipped_work: false   # local-only repository: skip the pushed-work check
```

## Step 2: Configure the Workspace

Bind the workspace to its bead backend. This writes `./.needle.yaml`, the global
`~/.config/needle/config.yaml` (only because it does not exist yet inside the
sandbox HOME — `needle init` refuses to touch an existing one without
`--force`), and a "Working with beads" section in `./AGENTS.md` for any coding
agent that works in this repo:

```bash
needle init --backend bead-rs
```

Then narrow the sandbox HOME's global config to the one-worker example by
**copying the shipped file**:

```bash
NEEDLE_REPO=/path/to/NEEDLE   # adjust to your checkout
cp "$NEEDLE_REPO/docs/examples/quickstart/config.yaml" "$HOME/.config/needle/config.yaml"
```

Copying instead of retyping is deliberate. The file lands at
`$HOME/.config/needle/config.yaml` only because HOME is the sandbox from
Step 0 — on a real NEEDLE host that path *is* the fleet's config, `needle init`
refuses to overwrite it without `--force`, and `needle doctor` warns when it is
byte-identical to this example while other NEEDLE workspaces live on the host.
The copy also keeps the content identical to what ships with this repo, which is
exactly what doctor compares against.

## Step 3: Initialize the Bead Store

```bash
# Initialize the bead store
bead init --prefix quickstart

# Verify everything resolves
needle doctor
```

**Expected `needle doctor` output** (real output from needle 0.6.0 + bead 0.2.6 on a clean host, 2026-09-09; paths shortened, disk figure elided):

```
NEEDLE Doctor
────────────────────────────────────────────────────────────
[PASS]  Config                        valid
[PASS]  Gate commands                 none configured
[PASS]  Workspace                     /tmp/needle-quickstart-project
[WARN]  SQLite integrity              sqlite3 not on PATH — skipped
[PASS]  Lock files                    none
[PASS]  DoD bypasses                  none recorded
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
16 passed, 2 warning(s), 0 failure(s).
Run `needle doctor --repair` to attempt automatic fixes.
```

Every row is `PASS` or `WARN` and the exit code is 0. A `FAIL` row names the fix.

## Step 4: Seed Test Beads

Run the provided seed script to create three test beads with one dependency:

```bash
# From the NEEDLE repo (same checkout NEEDLE_REPO pointed at in Step 2)
bash "$NEEDLE_REPO/docs/examples/quickstart/seed-beads.sh"
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
- Three git commits, one per bead, all pushed to the remote — `git log --oneline origin/main..HEAD` prints nothing

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
- Verify the branch has an upstream: `git rev-parse --abbrev-ref --symbolic-full-name @{u}`

**Worker logs `no upstream configured for branch '...'` (message captured from needle 0.6.0):**
The shipped-work gate could not verify a push because the branch has no upstream — the
workspace was created without step 1's `git remote add` + `git push -u`, or the remote was
removed afterwards. Give the branch an upstream (`git push -u origin <branch>`), or, for a
repository that will never have a remote, set `worker.enforce_shipped_work: false`. The
gate prints the remedy itself:

```
no upstream configured for branch 'main': the shipped-work gate cannot verify that the commit was pushed, so this closure is not counted as a failure. Remedy: `git push -u <remote> main` — that publishes the branch and sets its upstream in one step. If no remote exists yet, add one first: `git remote add origin <url>` (adding a remote that already exists is an error, so check `git remote -v` first).
```

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
# Leave the workspace and remove everything this example created
cd /
rm -rf /tmp/needle-quickstart-project /tmp/needle-quickstart-remote.git
rm -rf /tmp/needle-quickstart-home.*
```

Open a new terminal afterwards: your real HOME was never touched, and the
sandbox only ever existed inside that shell.

## Next Steps

- Try multiple workers: `needle run --agent claude --count 3`
- Note the built-in `claude` adapter runs with `--dangerously-skip-permissions` — expected for unattended work, but read `needle config` first
- Add more beads with dependencies: `bead dep add <dependent> <blocks>`
- Configure different agents in the sandbox HOME's `~/.config/needle/config.yaml` — on a real NEEDLE host that file is the fleet's config, so edit it deliberately rather than replacing it
- See [main README](https://github.com/jedarden/NEEDLE) for full documentation

---

**This is a teaching example.** The beads are simple, the config is minimal, and the project is disposable. Real-world use involves more complex workspaces, but the core loop — select, claim, dispatch, execute, outcome — is identical.
