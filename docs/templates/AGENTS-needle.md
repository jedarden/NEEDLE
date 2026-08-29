## Working with beads

Beads are tracked via the `bead` CLI (bead-rs). NEVER edit `.beads/` by hand — use the CLI.

### Core workflow

```bash
bead list --ready              # List claimable beads
bead show <id>                # Show bead details
bead claim <id>                # Claim a bead (sets assignee, status=in_progress)
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
