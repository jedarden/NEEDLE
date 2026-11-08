# lab bare-metal NEEDLE fleet — source-controlled policy

Owner for the lab (NixOS, 12 logical cores) NEEDLE worker fleet: the systemd
unit template, the resource slice, and the fleet membership manifest
(which workers exist, on which workspace, staggered by how much). Before
2026-09-10 all of this lived only in `~/.config` dotfiles on the host, so
"how many workers should lab run" had no answer anywhere in git.

Applied by [`apply-lab-fleet.sh`](apply-lab-fleet.sh); see that script for
what it deliberately never touches (credentials, systemd-managed drop-ins).

| File | Deploys to |
|---|---|
| `needle-worker@.service` | `~/.config/systemd/user/needle-worker@.service` |
| `trace-retention.{service,timer}` | scheduled inactive-workspace trace cleanup |
| `needle.slice` | `~/.config/systemd/user/needle.slice` |
| `workers.tsv` | rendered to `~/.config/needle/workers/<identifier>.env` |
| `fleet-policy.env` | `~/.config/needle/fleet-policy.env` |
| `bin/needle-checkpoint-unwedge` | `~/.local/bin/needle-checkpoint-unwedge` |
| `needle-checkpoint-sweep.{service,timer}` | fleet-wide checkpoint publication every 15 minutes |
| `bin/needle-checkpoint-sweep-watchdog` + `needle-checkpoint-sweep-watchdog.{service,timer}` | 5-minute timer health check and one-shot recovery |
| `../../scripts/{commit-checkpoint,checkpoint-publish}.sh` | `~/.local/lib/needle-checkpoint/` |
| `checkpoint-publication-{service,timer}.conf` | drop-ins for existing checkpoint publication units |
| `bin/cargo` | `~/.local/bin/cargo` (opt-in: `--install-cargo-wrapper`) |
| `bin/cargo-remote` | `~/.local/bin/cargo-remote` (same flag; hardening below) |
| `wrapper-drift.{service,timer}` | `~/.config/systemd/user/` + timer enabled (same flag) |
| `bin/bead` + `bead-routes.json` | `~/.local/bin/bead` + `~/.config/bead-route/routes.json` via `bin/install-bead-router.sh` (opt-in: `--install-bead-router`; see "bead-route") |

Checkpoint publication validates aligned durable state and coalesces standalone
commits for 15 minutes. It does not release claims, repair stores, or restart
workers. Upgrade workers to a build containing `1eaefdf8` before enabling that
cadence: older dispatch guards require Git publication after every mutation.
During a rolling upgrade, retain `NEEDLE_CHECKPOINT_COMMIT_DEBOUNCE_SECONDS=0`
on the publisher services until their workspace's workers have upgraded.

## Checkpoint sweep timer recovery

The lab sweep is a serialized `Type=oneshot` service using
`OnUnitActiveSec=15min`. That schedule is relative to activation of the service,
not an independent wall-clock interval. When the sweep exceeded its old
`TimeoutStartSec=1800` on 2026-10-30, systemd terminated the service and marked
it failed. The timer remained `active (elapsed)` but had no pending activation
(`NextElapseUSecMonotonic=infinity`): there was no successful service activation
from which systemd could calculate the next `OnUnitActiveSec` deadline. This is
why restarting only the timer did not help, while starting the service once did;
the service activation refreshed the baseline and the timer re-armed. The
codinghome sweep did not reproduce it because its serial workload completed
within the old timeout. Routed repositories are still skipped by the
bead-route guard and remain owned by their authority publisher.

Lab now allows one sweep to run for up to one hour and enables
`needle-checkpoint-sweep-watchdog.timer` on every converge. Every five minutes
the watchdog checks that the timer is active and has a finite next elapse. For
the broken state it starts the sweep service once with `--no-block`; a marker
under `~/.needle/checkpoint-sweep-watchdog/` prevents a retry storm while the
service is still broken or running. The marker is cleared after a finite next
elapse is observed, making one future incident recoverable. The watchdog does
not restart an active sweep and never touches routed work. Sharding remains a
future scale change if the one-hour bound is approached; the current mitigation
targets the observed 31-minute timeout without introducing concurrent
publishers.

## Wrapper tracking and drift detection (claudego-dd2fa6f4, 2026-09-24)

Both wrappers were untracked outside this directory until 2026-09-24, and
the consequence was real: on 2026-08-12 codinghome's `~/.local/bin/cargo`
was clobbered to a plain `~/.cargo/bin` symlink and stayed that way for five
weeks — every `cargo test` on the host ran real cargo locally, unbounded,
and nobody noticed until 2026-09-19. The lab's `cargo-remote` was still the
pre-hardening copy on 2026-09-24 (no `--slice`, no `RuntimeMaxSec`), so its
dirty-tree fallback scopes landed in `app.slice` and were never reaped.

`bin/cargo-remote` is the hardened copy (same two hardenings as `bin/cargo`:
`--slice="$(current_slice)"` so a `needle.slice` worker's fallback scope
stays inside the fleet ceiling, and `RuntimeMaxSec=14400` so a hung fallback
is reaped after 4h). The wrappers are deployed byte-identical on lab and
codinghome from here.

`wrapper-drift.timer` runs `bin/check-wrapper-drift.sh` every 30 minutes on
each host. It compares the deployed wrappers against `origin/main`'s copies
(after a fetch, so a stale checkout cannot hide upstream movement; checkout
tree as fallback) and **exits 1 on divergence**, failing the unit so the
journal and unit-failure monitors surface it. Exit 2 means the tracked copy
itself was unreachable — a blind detector, which fails harder. The service
executes the checker straight from this checkout, so the checker cannot
itself drift. Install: `apply-lab-fleet.sh --install-cargo-wrapper` on the
lab; on codinghome use `--wrappers-only`, which does only the wrapper +
drift-watch half — the fleet convergence above must never run there, since
this manifest is lab-specific.

## bead-route: one authoritative bead store per shared repo (needle-fdc34782)

**Why.** bead-rs keeps its live SQLite store per checkout. A repo cloned on
codinghome, lab and gmktec therefore had several live stores. Claims were
atomic only inside one host's store, so the same work ran twice, and two
stores published one checkpoint until it forked. On 2026-11-03,
declarative-config alone had 10 live copies.

**What.** `bin/bead` is a stdlib-only Python router installed as
`~/.local/bin/bead`, which is first on PATH for workers and agents on every
host. For each call it finds the workspace (the nearest ancestor of the cwd
holding `.beads/`) and normalizes its `origin` to `owner/repo`. It then looks
that up in the routing table (`bead-routes.json`, installed to
`~/.config/bead-route/routes.json`):

| Situation | What `bead` does |
|---|---|
| no routes file | pure pass-through (`exec` of the real binary) |
| unparseable routes file | refuses **every** call, exit 78 |
| unrouted repo, or no workspace | pass-through |
| authority host, authority path | pass-through, full access |
| authority host, another clone | runs the real bead in the authority path; client allowlist applies; input-file paths made absolute |
| host listed in `clients` | ships the call over multiplexed ssh to the authority's router (`__bead-route-serve__`); client allowlist applies |
| any other host | refuses, exit 78, naming the authority |

The real binary comes from `host.json`'s `real_bead`, then
`~/.local/lib/bead-route/bead-real`, then `~/.cargo/bin/bead`. The router
never resolves to itself. If none is usable, it exits 127.

The client allowlist covers what NEEDLE runs (`src/bead_store/backend.rs`,
the `capabilities --profile native-v1` and `--version` probes, and
`sync status|reconcile|flush-only` from `sync_guard.rs`) plus read-only and
ordinary issue commands. Clients are refused `init`, `restore`, `redact`,
`manifest`, `watchdog`, `sync import-only|fork|commit|configure|diff|bisect`,
`sync flush-only --output`, `doctor --repair|--rehearse|--starvation-recovery`,
and any unknown subcommand. The serve side re-checks the client host, the
workspace path and the allowlist. Input files
(`--description-file`, `--notes-file`, `--reason-file`, `--body-file`,
`--value-file`, `query --file`) travel inside the request. They are
materialized in a 0700 temp dir that is always removed. `-` and
`--fencing-token-stdin` forward stdin; stdin is otherwise never read, and a
terminal stdin never is. Only `BEAD_ACTOR` is forwarded from the client's
environment. The authority applies its own `BEAD_ORG_SECRET_SCANNER` from
`~/.config/environment.d` when the ssh session lacks it.

`bead __bead-route-role__ [DIR]` prints
`{"role": "unrouted|authority|redirect|client|refused", ...}`. The checkpoint
publishers (`needle-checkpoint-unwedge`, `needle-checkpoint-sweep`) query it
and skip any workspace whose role is not `authority` or `unrouted` (outcome
`routed`). Only the authority publishes and pushes a routed checkpoint. If a
routes file exists but the role query fails, they also skip (`route-unknown`,
fail safe).

**Install.** `apply-lab-fleet.sh --install-bead-router` on lab, or
`--wrappers-only --install-bead-router` on codinghome. Either runs
`bin/install-bead-router.sh [--dry-run]`, which:

1. validates the table;
2. preserves a non-router `~/.local/bin/bead` as `bead-real`, with a
   timestamped backup of a different previous copy;
3. writes `host.json` and `routes.json` atomically;
4. swaps the router in with an atomic rename;
5. verifies `bead --version` and the role query, and restores the previous
   binary if either fails.

There is never a moment without a working `bead`. Rerunning the installer is
idempotent.

**Onboarding a repo.**

1. Confirm that every non-authority copy holds no unique beads. Migrate or
   recreate any that do on the authority first.
2. Make sure the authority runs a bead at least as new as every client's.
3. Add `"owner/repo": {"authority_host", "authority_path", "clients"}` to
   `bead-routes.json`, then commit and push.
4. Rerun the installer on every host.

A diverged repo needs a decision before it is routed.

**gmktec.** Tailnet policy blocks gmktec from opening SSH connections. It can
therefore be an authority but never a client. Calls on gmktec for a repo whose
authority is elsewhere are refused (exit 78). Do that work on the authority or
a listed client. gmktec's workers are pinned to repos it is authority for.

**Failure modes.**

| Exit or signal | Cause | Action |
|---|---|---|
| 75 | The authority was unreachable (ssh exit 255). | The call is refused and never falls back to the local store. There is deliberately no disable switch. |
| 78 | Refused command, unlisted host, mismatched or unreadable origin, or an invalid routes file. | Fix the routes file. An invalid file refuses every call on that host. |
| 127 | No real bead binary was found. | Rerun the installer. |
| wrapper-drift DRIFT | A bead upgrade that writes `~/.local/bin/bead` replaced the router with a plain binary, so that host is unrouted again. `check-wrapper-drift.sh` compares `~/.local/bin/bead` and `routes.json` against the tracked copies and checks `real_bead` whenever `host.json` exists. | Rerun the installer. It adopts the new binary as `bead-real`. |

## CPU budget accounting (needle-3d5c65d8, documented 2026-09-10)

Launch admission (`src/rate_limit/mod.rs`) reads
`std::thread::available_parallelism()` for its core count. That call is
cgroup-v2-quota-aware, so **a worker inside `needle.slice` sees the slice
quota, not the host** — confirmed in the 2026-09-03 journal, where every
admission abort printed `/ 7 cores` on a 12-core host.

| Layer | Value | Where set |
|---|---|---|
| Host logical cores (`nproc`) | 12 | hardware |
| `user.slice` (all user processes) | `cpu.max` = 9 cores | host admin |
| `needle.slice` (fleet workers + their dispatches) | `CPUQuota=700%` = **7 cores**; `MemoryHigh=24G`, `MemoryMax=32G`, `TasksMax=1500` | `needle.slice` + `needle.slice.d/{cpu,limits}.conf` (folded into the tracked fragment) |
| Core count admission divides by | **7** (slice quota), not 12 | `available_parallelism()` in `src/rate_limit/mod.rs` |
| Effective CPU admission threshold | `worker.cpu_load_warn` 0.80 × 7 = defer above 1-min load **5.6** | `worker.cpu_load_warn` (config, stays enabled) |
| Agent-spawned `cargo` scopes | `CPUQuota=200%`, `MemoryMax=6G` per scope, in `app.slice` — **outside** the 7-core fleet budget | `~/.local/bin/cargo` wrapper |

Two consequences worth internalizing before touching any of these numbers:

1. **Admission is deliberately stricter than the host.** It defers new
   launches at load 5.6 on a 12-core box because only ~7 of those cores are
   the fleet's; the other 5 belong to the operator, monitoring, and
   everything in `app.slice`.
2. **`app.slice` cargo scopes are the budget hole.** They escape
   `needle.slice`, are capped only by `user.slice` (9 cores), and until
   2026-09-10 had **no time bound**.

## 2026-09-03 incident — mechanism and remediation

Five hung SIGIL `cargo test` binaries (`stream_collect*` tests that never
return) sat in abandoned `systemd-run` scopes, each entitled to 200% CPU.
Together they consumed ~7 of the 9 `user.slice` cores and pinned the 1-minute
load at ~7.0 — exactly the admission threshold, so every worker launch was
deferred. The then-deployed binary *aborted* after 4 deferrals (~125s), and
systemd's `Restart=always` / `RestartSec=30` brought it straight back: each
cycle took ~155-185s, slow enough that `StartLimitBurst=5` in 300s never
tripped. Fifteen services bounced for hours — restart counters reached
130-355, with 68 admission failures in 15 minutes. Historical context:
needle-a7e125a8 (2026-07-29, 115 leaked claude processes, load 300+).

Remediation, in three layers:

1. **Product (N-T33, separate bead, already shipped):** admission-blocked
   workers now *hold resident* and resume selection when the host recovers,
   instead of exiting. Lab runs needle 0.6.0 (01ecf05, 2026-09-08), which has
   this. This is why the current fleet shows zero restarts under load.
2. **Unit policy (this directory):** `RestartSec` 30s → 120s, start limit
   300s/5 → 900s/3, so any future fast-exit loop fails *visibly* after three
   cycles instead of herding systemd all day. The CPU/RAM admission threshold
   itself is untouched and must stay enabled — no
   `NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK` anywhere in these units.
3. **Leak bound:** `bin/cargo` adds `RuntimeMaxSec=14400` to the cgroup
   scope, so an orphaned cargo invocation is reaped by systemd after 4h
   instead of never (the SIGIL orphans had run 14-24 days).

Fleet size: **8 workers** (`workers.tsv`), against the 2026-09-09 right-size
of the 15-unit fleet down to 7 — one workspace family each, ~1 core per worker
against the 7-core slice, start-staggered 15s → 165s.

`lab-glm-loom` is the deliberate eighth worker, originally introduced as
`lab-codex-loom` (needle-f14e1ec3, 2026-10-02). On 2026-10-11,
needle-16368f14 reassigned that inactive slot to prepaid GLM-5.3-Flash and
renamed its identity to match the provider; the old Codex unit is disabled.
It is pinned to the lab-owned LOOM development queue and replaces the temporary
duplicate `lab-codex-tgplat` slot without increasing fleet size or the 7-core
slice. LOOM's production runtime and release control remain on their existing
hosts. The slot drops the average to ~0.875 cores/worker against the same
7-core slice, so it must be reassigned or retired rather than duplicated when
queue ownership changes again.

The lab Flash adapter has an 80-turn ceiling (needle-16368f14, 2026-10-11).
The first `lab-glm-loom` dispatch, `loom-b9a3a285`, exhausted the former
30-turn ceiling after reading plans, source, and related beads, with no source
changes or verification. The larger bounded budget leaves room for implementation
and required checks; provider, model, timeouts, resource caps, and admission
controls retain their existing values. NEEDLE caches adapter templates in its
dispatcher, so an adapter file edit takes effect after dispatcher reconstruction
at a safe claim boundary. A changed `agent.adapters_dir` also rebuilds the
dispatcher during configuration reload. A temporary equivalent path resolving
to the same directory can trigger that reload without a restart; restore its
original spelling after the worker acknowledges the applied key. The file edit
alone does not update an active attempt or guarantee adoption by its next claim;
confirm the new child process's `--max-turns 80` argument.

On 2026-10-05, `lab-gantry` switched from GLM-5.3-Flash to the existing Codex
adapter after 39 `max_turns` work failures in 24 hours and repeated upstream
429/5xx retries. This changes one route, not the worker count or slice budget.
Compare verified closures per worker-hour after the switch; restore the GLM
adapter in `workers.tsv` if the alternate route performs worse. The running
instance picks up the adapter only after its current claim finishes and the
unit is restarted.

On 2026-09-23, the idle `lab-needle` slot was reassigned to `lab-tgplat` to
move one build-heavy `tradegraph-platform` route off codinghome without adding
an eighth lab worker or raising the 7-core slice quota. The retired slot had no
active child and its local NEEDLE checkout had no initialized bead database;
codinghome already retains three NEEDLE-homed workers. The replacement remains
pinned (`explore=false`) so lab capacity cannot wander into browser-driving or
other host-incompatible repositories.

## 2026-09-10 orphan cleanup record

Every sustained process was attributed from read-only evidence (process
ancestry, scope → cgroup mapping, `~/.needle` logs, workspace bead stores)
before anything was stopped. 31 abandoned `run-p*.scope` units, spanning
2026-08-16 → 2026-09-08, all in workspaces with no remaining worker:

- 13 claude-dispatch scopes (SIGIL ×10, domain-check ×3): 9 of their beads
  already **Closed**; 3 InProgress with dead assignees (`claude-code-glm-4.7-lab-s1`,
  `claude-code-glm-4.7-lab-drawrace` — no such units; beads untouched for
  15-24 days).
- 18 cargo-test scopes: the test binary outlived its cargo parent inside the
  scope; each pairs with one of the attempts above.

Removed 2026-09-10 02:23 UTC via `systemctl --user stop` (systemd is the
scopes' owner; the underlying beads remain claimable). Post-stop: 0 scopes,
0 hung test binaries, all 7 workers still `active/running`, load 0.59.

Deliberately preserved: the two long-lived `claude` processes under
`herdr server` (operator sessions, not NEEDLE-owned), `lab-health-collector`
(fleet monitoring), and all 7 live workers.

Found during attribution, left for the owning repos: SIGIL `bf-1aqf5` and
`sigil-96ef0d42`, domain-check `bf-toud6j` are InProgress with assignees that
no longer exist — Mend will release them if those workspaces rejoin the
fleet.

## Operational notes

## Trace retention sweep

`trace-retention.timer` runs every 15 minutes after boot and invokes
`needle trace-retention --root /home/coding --json`. The command discovers real
`.beads/traces` workspaces in sorted order, loads each workspace's configured
failed/success retention windows, and skips workspaces with a live NEEDLE
worker, unfinished capture metadata, or any symlink in the trace tree. Actual
pruning/deletion is delegated to the same cleanup and `attempt_archive`
spooled-marker gate used by Mend. Successful captures retain `metadata.json`.

Deploy a NEEDLE build containing `trace-retention` to
`/home/coding/.local/bin/needle` first; the fleet script installs and enables
the systemd units but does not update that binary. Then install or converge
with `./fleet/lab/apply-lab-fleet.sh`; it enables the timer without restarting
workers. Review the JSON result and exit status with:

```text
systemctl --user status trace-retention.timer
journalctl --user -u trace-retention.service --since today --no-pager
needle trace-retention --root /home/coding --dry-run --json
```

The summary contains workspace/trace counts, reclaimed bytes, and an error
count only; trace contents are never printed. A nonzero error count fails the
oneshot so the timer's journal state is visible to fleet monitoring.

- `workers.tsv` pins every worker with `NEEDLE_WS`; roaming (`--workspace`
  dropped) is disabled on purpose until the claude-governor queue is triaged
  (see the comment in the unit template).
- The unit's `ExecStart` is `~/.local/bin/needle`, but a running worker may
  re-exec into `~/.needle/bin/needle-stable` (upgrade channel;
  `src/supervisor/binary_freshness.rs` watches it). `ps` showing
  `needle-stable` under a `needle-worker@` unit is expected.
- Converge without interrupting attempts: `apply-lab-fleet.sh` only
  daemon-reloads. Restart an instance explicitly to pick up unit changes.
- Releases install themselves: `needle-release-upgrade.timer` (shared with
  codinghome, from `fleet/ex44/`) checks GitHub every 6 hours. It verifies the
  checksums and installs through `needle upgrade --from-file`: :testing, then
  the canary suite in `~/.needle/canary`, then :stable. Running workers re-exec
  the promoted binary at a bead boundary, and a failed canary leaves :stable
  untouched. Check it with
  `journalctl --user -u needle-release-upgrade.service`; run it now with
  `systemctl --user start needle-release-upgrade.service`.
- Rollback: every replaced file is backed up on the host with a timestamp
  suffix, and this directory is git history.
- References to `bench` and `ex44` in inherited comments are historical;
  ex44 was decommissioned (see CLAUDE.md, "This box's identity").
