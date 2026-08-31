# Expected Output Examples

This file shows what a healthy NEEDLE quickstart run looks like, including the `needle doctor` output and a successful worker session.

## `needle doctor` Output (All Checks Pass)

Real output from needle 0.6.0 with bead 0.2.4, immediately after Step 4 (paths
shortened, disk figure elided). The two `WARN` rows are normal on a fresh host:
`sqlite3` is optional, and the heartbeat directory appears when the first worker
starts.

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

Exit code is `0`. A `FAIL` row names its fix — for example, an unbound workspace
says `set bead_cli.backend in .needle.yaml`, and a missing backend says which
binary to install.

## Healthy Worker Session

> Illustrative. The doctor block above is captured output; the worker session
> below shows the shape of a run, not a verbatim transcript.

### Starting the Worker

```bash
$ needle run --agent claude -i alpha

[2026-08-29 12:34:56] NEEDLE worker starting...
[2026-08-29 12:34:56] Worker identity: needle-claude-alpha
[2026-08-29 12:34:56] Workspace: /tmp/needle-quickstart-project
[2026-08-29 12:34:56] Agent: claude
[2026-08-29 12:34:56] Attaching to tmux session: needle-alpha-quickstart-1693302896
```

### Processing First Bead

```
[2026-08-29 12:34:57] 🔍 SELECT: querying bead store...
[2026-08-29 12:34:57]    Found 3 open beads
[2026-08-29 12:34:57]    Ready frontier: 2 beads (1 blocked by dependency)
[2026-08-29 12:34:57]    Selected: qs-abc123 (priority 2, created 2026-08-29T12:30:00Z)

[2026-08-29 12:34:57] 🔒 CLAIM: attempting atomic claim...
[2026-08-29 12:34:57]    Claim successful: qs-abc123 → in_progress
[2026-08-29 12:34:57]    Assignee: needle-claude-alpha

[2026-08-29 12:34:57] 📋 BUILD: constructing prompt...
[2026-08-29 12:34:57]    Bead: Add CONTRIBUTING.md
[2026-08-29 12:34:57]    Workspace: /tmp/needle-quickstart-project
[2026-08-29 12:34:57]    Context: README.md (existing)

[2026-08-29 12:34:58] 🚀 DISPATCH: invoking agent...
[2026-08-29 12:34:58]    Agent: claude
[2026-08-29 12:34:58]    Command: claude --print
[2026-08-29 12:34:58]    Timeout: 300s

[2026-08-29 12:34:58] ⏳ EXECUTE: agent running...
[2026-08-29 12:35:45]    Agent exited: code 0 (success)
[2026-08-29 12:35:45]    Duration: 47.2s
[2026-08-29 12:35:45]    Tokens: input=1234 output=5678

[2026-08-29 12:35:45] 📊 OUTCOME: processing success...
[2026-08-29 12:35:45]    Validating output...
[2026-08-29 12:35:45]    ✓ Changes detected: CONTRIBUTING.md (new file)
[2026-08-29 12:35:45]    ✓ Git commit created
[2026-08-29 12:35:45]    Closing bead: qs-abc123
[2026-08-29 12:35:45]    ✓ Closed successfully

[2026-08-29 12:35:45] ─── Cycle complete in 48.1s ───
```

### Processing Second Bead

```
[2026-08-29 12:35:46] 🔍 SELECT: querying bead store...
[2026-08-29 12:35:46]    Found 2 open beads
[2026-08-29 12:35:46]    Ready frontier: 1 bead (1 blocked by dependency)
[2026-08-29 12:35:46]    Selected: qs-def456 (priority 2, created 2026-08-29T12:30:15Z)

[2026-08-29 12:35:46] 🔒 CLAIM: attempting atomic claim...
[2026-08-29 12:35:46]    Claim successful: qs-def456 → in_progress

[2026-08-29 12:35:46] 📋 BUILD: constructing prompt...
[2026-08-29 12:35:46]    Bead: Add LICENSE file
[2026-08-29 12:35:46]    Context: README.md, CONTRIBUTING.md (existing)

[2026-08-29 12:35:47] 🚀 DISPATCH: invoking agent...

[2026-08-29 12:36:30] ⏳ EXECUTE: agent running...
[2026-08-29 12:36:30]    Agent exited: code 0 (success)
[2026-08-29 12:36:30]    Duration: 43.8s

[2026-08-29 12:36:30] 📊 OUTCOME: processing success...
[2026-08-29 12:36:30]    Validating output...
[2026-08-29 12:36:30]    ✓ Changes detected: LICENSE (new file)
[2026-08-29 12:36:30]    ✓ Git commit created
[2026-08-29 12:36:30]    Closing bead: qs-def456
[2026-08-29 12:36:30]    ✓ Closed successfully

[2026-08-29 12:36:30] ─── Cycle complete in 44.4s ───
```

### Processing Third Bead (After Dependency Unblocks)

```
[2026-08-29 12:36:31] 🔍 SELECT: querying bead store...
[2026-08-29 12:36:31]    Found 1 open beads
[2026-08-29 12:36:31]    Ready frontier: 1 bead (dependency qs-def456 now closed)
[2026-08-29 12:36:31]    Selected: qs-ghi789 (priority 1, created 2026-08-29T12:30:30Z)

[2026-08-29 12:36:31] 🔒 CLAIM: attempting atomic claim...
[2026-08-29 12:36:31]    Claim successful: qs-ghi789 → in_progress

[2026-08-29 12:36:31] 📋 BUILD: constructing prompt...
[2026-08-29 12:36:31]    Bead: Add simple Makefile
[2026-08-29 12:36:31]    Context: README.md, CONTRIBUTING.md, LICENSE (existing)

[2026-08-29 12:36:32] 🚀 DISPATCH: invoking agent...

[2026-08-29 12:37:15] ⏳ EXECUTE: agent running...
[2026-08-29 12:37:15]    Agent exited: code 0 (success)
[2026-08-29 12:37:15]    Duration: 43.1s

[2026-08-29 12:37:15] 📊 OUTCOME: processing success...
[2026-08-29 12:37:15]    Validating output...
[2026-08-29 12:37:15]    ✓ Changes detected: Makefile (new file)
[2026-08-29 12:37:15]    ✓ Git commit created
[2026-08-29 12:37:15]    Closing bead: qs-ghi789
[2026-08-29 12:37:15]    ✓ Closed successfully

[2026-08-29 12:37:15] ─── Cycle complete in 44.1s ───
```

### Worker Exits (Queue Empty)

```
[2026-08-29 12:37:16] 🔍 SELECT: querying bead store...
[2026-08-29 12:37:16]    Found 0 open beads
[2026-08-29 12:37:16]    Ready frontier: 0 beads
[2026-08-29 12:37:16]    Queue empty, evaluating strands...

[2026-08-29 12:37:16] 🪡 Pluck: no claimable beads in workspace
[2026-08-29 12:37:16] 🔧 Mend: no cleanup needed
[2026-08-29 12:37:16] 🔭 Explore: no other workspaces configured
[2026-08-29 12:37:16] 🪢 Knot: all strands exhausted

[2026-08-29 12:37:16] ──────────────────────────────────────────
[2026-08-29 12:37:16] Queue empty — no work remaining
[2026-08-29 12:37:16] Idle action: Exit
[2026-08-29 12:37:16] Shutting down worker...
[2026-08-29 12:37:16] Detaching from tmux session
[2026-08-29 12:37:16] ═══════════════════════════════════════════
[2026-08-29 12:37:16] ✅ NEEDLE worker session complete
[2026-08-29 12:37:16] ═══════════════════════════════════════════
    Total runtime: 2m 20s
    Beads processed: 3
    Success rate: 100%
    Total agent cost: $0.012 (approx)
```

## Verifying Results

```bash
$ bead list --status closed

ID           TITLE                    STATUS    CLOSED_AT
qs-abc123    Add CONTRIBUTING.md     closed    2026-08-29T12:35:45Z
qs-def456    Add LICENSE file         closed    2026-08-29T12:36:30Z
qs-ghi789    Add simple Makefile      closed    2026-08-29T12:37:15Z

$ git log --oneline

a1b2c3d fix(qs-ghi789): add simple Makefile
d4e5f6g fix(qs-def456): add LICENSE file
h8i9j0k fix(qs-abc123): add CONTRIBUTING.md
l1m2n3o Initial commit

$ ls -la

drwxr-xr-x  .beads/
-rw-r--r--  CONTRIBUTING.md
-rw-r--r--  LICENSE
-rw-r--r--  Makefile
-rw-r--r--  README.md
-rw-r--r--  .needle.yaml
```

## What to Look For

✅ **Healthy indicators:**
- `needle doctor` shows all `✓` marks
- Worker cycles through SELECT → CLAIM → BUILD → DISPATCH → EXECUTE → OUTCOME
- Each bead transitions `open → in_progress → closed`
- Worker exits cleanly when queue is empty
- Git commits are created with bead IDs in trailers
- All three files are created in the workspace

❌ **Warning signs:**
- Beads stuck in `in_progress` (agent crashed or hung)
- Worker exits immediately (no beads, config error)
- `needle doctor` shows `✗` marks (missing dependencies)
- No git commits created (validation failed)
- Files missing (agent didn't produce expected output)
