## Working with beads

Beads are tracked via the `bead` CLI (bead-rs). NEVER edit `.beads/` by hand — use the CLI.

### Core workflow

```bash
bead list --ready              # List claimable beads
bead show <id>                # Show bead details
bead claim                     # Claim the next ready bead (no id: selection is server-side)
bead update <id> --status in_progress --assignee <you>   # Take a specific bead
bead update <id> --notes "..."# Add notes without changing status
bead close <id> --reason "..." # Complete a bead (requires reason)
bead release <id>             # Release bead back to ready frontier
```

### Commit discipline

**One commit per bead, always.** Every commit MUST include the bead ID in the trailer:

```bash
git commit -m "fix(needle-XYZ): description" -m "Bead-Id: needle-XYZ"
```

NEEDLE enforces this via `enforce_shipped_work`: a close without a matching commit reopens the bead.

### Dependencies

Block a bead until another completes:

```bash
bead dep add <BLOCKED_ID> <BLOCKER_ID>
```

### NEEDLE exit codes

- `0`: Success → bead closes
- `1`: Failure → bead stays in_progress, agent continues
- Other codes are treated as failure

### Troubleshooting

Run `needle doctor` for diagnostics:
- Bead backend health
- Workspace configuration
- Stuck beads and locks
- Worker status

For backend issues, run `bead doctor` directly.

### Never bypass the Definition of Done

`git commit --no-verify` is not a tool. The pre-commit hook logs every bypass to
`.beads/bypasses.jsonl` with the commit sha, and NEEDLE's `no-dod-bypass` gate
fails the dispatch of any bead whose commits appear there — the bead is
released, not closed, and the failure counts toward quarantine. If the hook is
red because of someone else's edits in the shared tree, verify on a clean
extraction (`git archive HEAD | tar -x -C "$(mktemp -d)"`), then commit with
the hook enabled. A red CI never makes a bypass acceptable; it makes the
build-fix bead the only claimable work.
