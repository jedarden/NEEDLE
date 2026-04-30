# Idle Strand Gating Semantics

## Overview

NEEDLE workers run idle-time strands (reflect, weave, unravel, pulse) when no beads are available for processing. These strands have **per-workspace gates** that affect all workers scanning the same workspace.

## Per-Workspace vs Per-Worker Gating

All idle-time strands use **workspace-level state** stored in `~/.needle/state/<strand>/<workspace-hash>.json`. This means:

- When one worker triggers a strand (e.g., reflect consolidates learnings), **all subsequent workers** on that workspace see the same cooldown state.
- A fleet of 20 workers scanning 10 workspaces can land where all gates are closed simultaneously, causing zero idle-time work globally.

### Strand Gating Details

| Strand | State File | Cooldown | Gate Scope | Key Semantics |
|--------|-----------|----------|------------|---------------|
| **Reflect** | `reflect/reflect_state.json` | 24 hours | Per-workspace (single file) | Tracks last consolidation timestamp and bead count. Requires 10+ beads closed since last run. |
| **Weave** | `weave/<hash>.json` | 24 hours | Per-workspace | Tracks last run and seen bead titles (dedup). All workers share the same last_run timestamp. |
| **Pulse** | `pulse/<hash>.json` | 48 hours | Per-workspace | Tracks last run and seen issue fingerprints. All workers share cooldown state. |
| **Unravel** | `unravel/<hash>.json` | 168 hours (7 days) | Per-workspace | Tracks per-bead analysis timestamps. Each bead has its own cooldown, but state is shared. |
| **Pluck** | None | None | No gating | Runs on every cycle, no state persistence. |
| **Explore** | None | None | No gating | Runs on every cycle against configured workspace list. |
| **Mend** | None | None | No gating | Runs on every cycle, maintains orphan detection via heartbeats. |

## Observing Idle Strand State

Use `needle status --idle-strands` to see cooldown state across all configured workspaces:

```bash
needle status --idle-strands
```

Output shows:
- **STRAND**: Strand name
- **WORKSPACE**: Workspace path
- **ENABLED**: Whether the strand is enabled in config
- **LAST RUN**: Timestamp of last run (or "never")
- **STATUS**: Current status ("ready", "in cooldown", or "disabled")

JSON format:
```bash
needle status --idle-strands --format json
```

## Fleet Considerations

### Problem: Simultaneous Gate Closure

When scaling to 20 workers across 10 workspaces:
1. First worker triggers reflect → all workers on that workspace see 24h cooldown
2. Same for weave, pulse, unravel
3. Result: idle workers have nothing to do despite `idle_action=wait`

### Mitigation Strategies

1. **Stagger cooldowns**: Configure different cooldown values per workspace if possible.
2. **Enable selectively**: Only enable idle strands on a subset of workspaces.
3. **Monitor via `--idle-strands`**: Use `needle status --idle-strands` to verify gates aren't all closed simultaneously.
4. **Scale appropriately**: If all gates are closed, additional workers won't produce idle-time work.

## Telemetry

All strands emit telemetry events regardless of result:
- `strand.evaluated` → `{strand_name, result, duration_ms}` where result is one of: `bead_found`, `work_created`, `no_work`, `error`
- `strand.skipped` → `{strand_name, reason}` when a strand is disabled or gated

This means operators can see exactly what each strand decided, even when returning `NoWork`.

## State File Locations

```
~/.needle/state/
├── reflect/
│   └── reflect_state.json          # Single shared file
├── weave/
│   ├── <hash1>.json                 # One per workspace
│   └── <hash2>.json
├── pulse/
│   ├── <hash1>.json
│   └── <hash2>.json
└── unravel/
    ├── <hash1>.json
    └── <hash2>.json
```

Where `<hash>` is the first 16 hex characters of SHA-256(workspace_path).
