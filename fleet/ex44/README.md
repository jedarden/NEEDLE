# ex44 NEEDLE fleet policy

This directory makes the codinghome/ex44 worker capacity and backlog policy
reproducible. It targets 21 workers against the shared Z.ai proxy: 18 pinned
workers for the busiest or highest-leverage repositories and 3 roaming workers
for the maintained-workspace frontier. Nine workers use GLM-5.3 and twelve use
GLM-5.3-Flash, staying below the configured per-model ceilings.

## What this implements

1. **Rehabilitate the queue.** The deployed worker uses expired-quarantine
   recovery, so automatically quarantined beads return to selection after the
   hold expires. Human/manual holds remain hard exclusions.
2. **Finish high-leverage blockers first.** Pluck orders pinned candidates by
   open-dependent impact before normal priority/age tie-breakers.
3. **Fix routing.** `workers.tsv` removes the superseded CLASP route, pins the
   SEAM and irreversible-command-gate workers correctly, and adds three roaming
   workers. `required-explore-workspaces.txt` restores maintained FABRIC to the
   explicit roaming set.
4. **Replenish automatically.** `fleet-policy.env` enables the low-water
   generation gate with six eligible beads in reserve and a five-minute
   workspace/strand lease, preventing a thundering herd of generators. The
   gate is enabled for pinned pools and disabled for roaming identities: a
   roamer consumes the shared frontier instead of inventing work in its
   arbitrary home repository.
5. **Enforce a backlog SLO.** For 21 workers, the nominal target is 84
   eligible beads (four per worker) and the minimum is 42 (two per worker).
   The verdict is route-aware: every pinned repository must cover its assigned
   workers, and the roaming pool must have enough residual work after pinned
   reservations. This prevents a large NEEDLE queue from hiding an idle SEAM
   worker. `needle-backlog-slo.timer` measures the actual frontier every five
   minutes and emits the under-provisioned routes in JSON.
6. **Improve task yield.** Full agent-wallclock timeouts trigger Mitosis once
   90% of the configured timeout has elapsed. New beads should describe one
   bounded deliverable, name an executable acceptance check, and use dependency
   edges for work that touches the same file or function.

The backlog audit intentionally counts selection eligibility rather than raw
open beads. It excludes active timed holds, manual/human work, and ordinary
deferred work while admitting an expired automatic quarantine marking.

## Validate and apply

```bash
fleet/ex44/test.sh
fleet/ex44/apply-ex44-fleet.sh --dry-run
fleet/ex44/backlog-slo.sh --table
fleet/ex44/apply-ex44-fleet.sh --start-new --retire-strays
```

The apply script backs up changed host files, enables exactly the manifest
units, starts only inactive workers when requested, and never restarts a
running worker. Route changes therefore take effect naturally at the next safe
service restart. `--retire-strays` disables and masks non-manifest instances
without stopping their current process.

Worker credentials remain in the host-only `worker-common.env`; neither the
manifest nor the shared policy contains secrets. To roll back, restore the
timestamped files under `~/.config/systemd/user` and `~/.config/needle`, run
`systemctl --user daemon-reload`, then restart only idle worker instances.

`needle-zai-governor` protects the proxy without fighting the manifest. It
scales only the four expansion workers (`glm-icg` and `glm-roam-18` through
`20`) between one and four. The pinned ICG route is first in the pool and is
therefore preserved by the one-worker floor; pressure sheds roamers first.
Three or more 429 retries in a 100-second window remove one worker; a clean
window adds one. The separate 17-worker base fleet is never disabled by this
controller.
