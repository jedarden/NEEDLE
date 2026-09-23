# NEEDLE upgrades

`needle upgrade` uses the release channels so a downloaded GitHub artifact is
never installed directly over the running executable:

```text
GitHub release → needle-testing → canary gate → needle-stable → hot reload
```

The candidate is written atomically to `~/.needle/bin/needle-testing`. NEEDLE
runs the configured canary workspace and promotes the candidate only when the
entire suite passes. Promotion moves the previous stable binary to
`~/.needle/bin/needle-stable.prev`; workers notice the new stable hash at a
safe cycle boundary and re-exec into it without losing their worker identity.

If the canary fails, times out, or cannot be run, the candidate is rejected and
both `needle-stable` and `needle-stable.prev` remain unchanged. Configure the
canary location and timeout with:

```yaml
self_modification:
  canary_workspace: ~/.needle/canary
  canary_timeout: 1800
```

Inspect or recover the channels with:

```bash
needle canary --status
needle rollback
```

`needle upgrade --from-file PATH` follows the same channel and gate for a
local build. `--skip-canary` is an explicit bootstrap or emergency override
for local artifacts only; it is not used for GitHub releases. Do not copy or
move a binary directly over `~/.needle/bin/needle-stable` while workers run.

The supervisor can run the same GitHub-release pipeline periodically. This is
opt-in and defaults to disabled:

```yaml
supervisor:
  auto_upgrade_check: false
  update_check_interval_secs: 21600 # six hours
```
