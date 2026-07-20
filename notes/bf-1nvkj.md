# Operational Fix: Clear explore.workspaces Static List

**Date:** 2026-07-20
**Bead:** bf-1nvkj

## Change

Cleared the `strands.explore.workspaces` configuration in `/home/coding/.config/needle/config.yaml` from a static list of 24 paths to an empty list `[]`.

## Before

The config contained a stale static enumeration of 24 workspace paths:
- miroir, HOOP, SIGIL, pdftract, bead-forge, ai-code-battle, spaxel, FABRIC
- drawrace, mobile-gaming, vista, NEEDLE, gribtract, mta-my-way, claude-governor
- aide-de-camp, zai-proxy, claude-print, domain-check, AgentScribe
- telegram-claude-bridge, ARMOR, sun-sim, pose-detection

This static list was missing at least two repos that exist under `workspace_root`:
- `commitgraph`
- `twitterapi-proxy`

## After

```yaml
strands:
  explore:
    enabled: true
    workspaces: []        # Now empty - lets discover_workspaces() run
    workspace_root: /home/coding/
```

## Rationale

The `discover_workspaces()` function already automatically enumerates all git repos under `workspace_root`. The static list was redundant and stale - it represented a historical snapshot rather than a deliberate pin. By clearing it, workers now use the default discovery behavior which:

1. Automatically detects new repos added to `/home/coding/`
2. Removes the need for manual maintenance of the workspace list
3. Ensures all current repos (including commitgraph and twitterapi-proxy) are available for exploration

## Verification

- Config confirmed: `workspaces: []`
- Workers restarted and running
- Verified `commitgraph` and `twitterapi-proxy` exist under `/home/coding/`

## Notes

The `workspaces` field is retained in the config (not removed) so operators can still deliberately pin specific workspaces if needed. The empty list restores the default discovery behavior.
