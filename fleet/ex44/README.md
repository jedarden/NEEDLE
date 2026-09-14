# ex44 NEEDLE fleet policy

This directory makes the codinghome/ex44 worker capacity and backlog policy
reproducible. It targets up to 25 workers against the shared Z.ai proxy: 11
home-only workers and 14 workers allowed to roam across the maintained-workspace
frontier. Nine workers use GLM-5.3 and up
to sixteen use GLM-5.3-Flash, with live-session concurrency enforced separately
from the number of registered workers.

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
   workspace/strand lease, preventing a thundering herd of generators. Pinned
   workers replenish their home repository; roaming workers use the same
   approved workspace set as Explore, skip healthy or contended repositories,
   and run at most one creative pass per selection cycle.
5. **Enforce a backlog SLO.** For 25 workers, the nominal target is 100
   eligible beads (four per worker) and the minimum is 50 (two per worker).
   Fresh heartbeats raise those thresholds when temporary workers are also
   deployed, so extra consumers cannot be hidden behind the steady-state
   manifest count.
   The verdict is route-aware: every pinned repository must cover its assigned
   workers, and the roaming pool must have enough residual work after pinned
   reservations. This prevents a large NEEDLE queue from hiding an idle SEAM
   worker. `needle-backlog-slo.timer` measures the actual frontier every five
   minutes and emits the under-provisioned routes in JSON.
6. **Improve task yield.** Full agent-wallclock timeouts trigger Mitosis once
   90% of the configured timeout has elapsed. New beads should describe one
   bounded deliverable, name an executable acceptance check, and use dependency
   edges for work that touches the same file or function.
7. **Recover stuck routes.** Workers whose home queues repeatedly fail before
   Explore use the managed `~/.needle/roam-only` home. It has an explicit bead
   backend binding and no bead store, so home strands skip cleanly and Explore
   selects work from the maintained-workspace allowlist.

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
scales the eight expansion workers (`glm-icg` and `glm-roam-18` through `24`)
between one and eight, for 18--25 active workers including the fixed base. The
pinned ICG route is first in the pool and is therefore preserved by the
one-worker floor; pressure sheds roamers first. A productive window with no
recoverable 429 adds one worker. One independently affected request holds the
quota boundary; multiple affected requests or any terminal 429 remove one.
The separate 17-worker base fleet is never disabled by this controller.
Scale-down disables the excess unit immediately but stops it only after NEEDLE
reports `EXHAUSTED` with no current bead; selectors and executors drain instead
of abandoning an epoch-fenced claim. A v3 state file starts at the prior
four-worker expansion ceiling, so the four new roamers are probed one per clean
window rather than enabled together.
