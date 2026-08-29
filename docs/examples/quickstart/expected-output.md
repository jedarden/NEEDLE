# Expected Output Examples

This file shows what a healthy NEEDLE quickstart run looks like, including the `needle doctor` output and a successful worker session.

## `needle doctor` Output (All Checks Pass)

```
✓─────────────────────────────────────────────────────────────────✓
NEEDLE Doctor: Configuration and Dependency Check
✓─────────────────────────────────────────────────────────────────✓

[✓] Bead backend detection
    backend:          bead-rs
    binary:           /home/user/.local/bin/bead
    version:          bead-rs 0.2.0

[✓] Bead store initialization
    store:            /tmp/needle-quickstart-project/.beads/beads.db
    checkpoint:       .beads/checkpoint/current.json
    status:           Valid

[✓] Workspace configuration
    config:           /tmp/needle-quickstart-project/.needle.yaml
    backend:          bead-rs
    workspaces:       1 configured

[✓] Agent CLI availability
    agent:            claude
    path:             /home/user/.local/bin/claude
    version:          claude 1.2.3

[✓] Configuration validation
    global config:    ~/.config/needle/config.yaml
    worker config:    Valid
    agent config:     Valid

[✓] Telemetry (optional)
    otlp_sink:        Not configured (optional)

═════════════════════════════════════════════════════════════════════
✓ All checks passed — your workspace is ready for NEEDLE workers
═════════════════════════════════════════════════════════════════════
```

## Healthy Worker Session

### Starting the Worker

```bash
$ needle run --agent claude --identity alpha

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
